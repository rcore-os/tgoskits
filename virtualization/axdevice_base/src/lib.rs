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

//! Basic traits and structures for emulated devices in ArceOS hypervisor.
//!
//! This crate provides the foundational abstractions for implementing virtual devices
//! in the [AxVisor](https://github.com/arceos-hypervisor/axvisor) hypervisor. It is
//! designed for `no_std` environments and supports multiple architectures.
//!
//! # Overview
//!
//! The crate contains the following key components:
//!
//! - [`BaseDeviceOps`]: The core trait that all emulated devices must implement.
//! - [`EmuDeviceType`]: Enumeration representing the type of emulator devices
//!   (re-exported from `axvmconfig` crate).
//! - [`EmulatedDeviceConfig`]: Configuration structure for device initialization.
//! - Trait aliases for specific device types:
//!   - [`BaseMmioDeviceOps`]: For MMIO (Memory-Mapped I/O) devices.
//!   - [`BaseSysRegDeviceOps`]: For system register devices.
//!   - [`BasePortDeviceOps`]: For port I/O devices.
//!
//! # Usage
//!
//! To implement a custom emulated device, you need to implement the [`BaseDeviceOps`]
//! trait with the appropriate address range type:
//!
//! ```rust,ignore
//! use axdevice_base::{BaseDeviceOps, EmuDeviceType};
//! use axaddrspace::{GuestPhysAddrRange, device::AccessWidth};
//! use ax_errno::AxResult;
//!
//! struct MyDevice {
//!     base_addr: usize,
//!     size: usize,
//! }
//!
//! impl BaseDeviceOps<GuestPhysAddrRange> for MyDevice {
//!     fn emu_type(&self) -> EmuDeviceType {
//!         EmuDeviceType::Dummy
//!     }
//!
//!     fn address_range(&self) -> GuestPhysAddrRange {
//!         (self.base_addr..self.base_addr + self.size).try_into().unwrap()
//!     }
//!
//!     fn handle_read(&self, addr: GuestPhysAddr, width: AccessWidth) -> AxResult<usize> {
//!         // Handle read operation
//!         Ok(0)
//!     }
//!
//!     fn handle_write(&self, addr: GuestPhysAddr, width: AccessWidth, val: usize) -> AxResult {
//!         // Handle write operation
//!         Ok(())
//!     }
//! }
//! ```
//!
//! # Feature Flags
//!
//! This crate currently has no optional feature flags. All functionality is available
//! by default.

#![no_std]
#![feature(trait_alias)]
// trait_upcasting has been stabilized in Rust 1.86, but we still need a while to update the minimum
// Rust version of Axvisor.
#![allow(stable_features)]
#![feature(trait_upcasting)]
#![allow(incomplete_features)]
#![feature(generic_const_exprs)]
#![warn(missing_docs)]

extern crate alloc;

mod device;

use alloc::{string::String, sync::Arc, vec::Vec};
use core::any::Any;

pub use ax_errno::AxResult;
pub use axvm_types::{
    EmulatedDeviceType as EmuDeviceType, GuestPhysAddr, GuestPhysAddrRange, InterruptTriggerMode,
    IrqLineId,
};

pub use crate::device::{
    AccessWidth, BusAccess, BusKind, BusResponse, DeviceAddr, DeviceAddrRange, DeviceError, Port,
    PortRange, SysRegAddr, SysRegAddrRange,
};

/// Represents the configuration of an emulated device for a virtual machine.
///
/// This structure holds all the necessary information to initialize and configure
/// an emulated device, including its memory mapping, interrupt configuration, and
/// device-specific parameters.
///
/// # Fields
///
/// - `name`: A human-readable identifier for the device.
/// - `base_ipa`: The starting address in guest physical address space.
/// - `length`: The size of the device's address space in bytes.
/// - `irq_id`: The interrupt line number for device interrupts.
/// - `emu_type`: Numeric identifier for the device type.
/// - `cfg_list`: Device-specific configuration parameters.
///
/// # Example
///
/// ```rust
/// use axdevice_base::EmulatedDeviceConfig;
///
/// let config = EmulatedDeviceConfig {
///     name: "uart0".into(),
///     base_ipa: 0x0900_0000,
///     length: 0x1000,
///     irq_id: 33,
///     emu_type: 1,
///     cfg_list: vec![115200], // baud rate
/// };
/// ```
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct EmulatedDeviceConfig {
    /// The name of the device.
    ///
    /// This is a human-readable identifier used for logging, debugging, and
    /// device tree generation. It should be unique within a virtual machine.
    pub name: String,

    /// The base IPA (Intermediate Physical Address) of the device.
    ///
    /// This is the starting address in the guest's physical address space
    /// where the device's registers are mapped. The guest OS will use this
    /// address to access the device.
    pub base_ipa: usize,

    /// The length of the device's address space in bytes.
    ///
    /// This defines the size of the memory region that the device occupies.
    /// Any access within `[base_ipa, base_ipa + length)` will be routed to
    /// this device.
    pub length: usize,

    /// The IRQ (Interrupt Request) ID of the device.
    ///
    /// This is the interrupt line number that the device uses to signal
    /// events to the guest. The value should correspond to a valid interrupt
    /// ID in the virtual interrupt controller.
    pub irq_id: usize,

    /// The type of emulated device.
    ///
    /// This numeric value identifies the device type and is used by the
    /// device manager to instantiate the correct device implementation.
    /// See [`EmuDeviceType`] for predefined device types.
    pub emu_type: usize,

    /// Device-specific configuration parameters.
    ///
    /// This is a list of configuration values whose meaning depends on the
    /// specific device type. For example, a UART device might use this to
    /// specify baud rate, while a virtio device might use it for queue sizes.
    pub cfg_list: Vec<usize>,
}

/// The core trait that all emulated devices must implement.
///
/// This trait defines the common interface for all virtual devices in the hypervisor.
/// It provides methods for device identification, address range querying, and
/// handling read/write operations from the guest.
///
/// # Type Parameters
///
/// - `R`: The address range type that the device uses. This determines the
///   addressing scheme (MMIO, port I/O, system registers, etc.).
///
/// # Implementation Notes
///
/// - All implementations must also implement [`Any`] to support runtime type checking.
/// - The `handle_read` and `handle_write` methods are called by the hypervisor's
///   trap handler when the guest accesses the device's address range.
/// - Implementations should handle concurrent access appropriately if the device
///   can be accessed from multiple vCPUs.
///
/// # Example
///
/// See the crate-level documentation for a complete implementation example.
pub trait BaseDeviceOps<R: DeviceAddrRange>: Any {
    /// Returns the type of the emulated device.
    ///
    /// This is used by the device manager to identify the device type and
    /// perform type-specific operations.
    fn emu_type(&self) -> EmuDeviceType;

    /// Returns the address range that this device occupies.
    ///
    /// The returned range is used by the hypervisor to route guest memory
    /// accesses to the appropriate device handler.
    fn address_range(&self) -> R;

    /// Handles a read operation on the emulated device.
    ///
    /// # Arguments
    ///
    /// - `addr`: The address within the device's range being read.
    /// - `width`: The access width (byte, halfword, word, or doubleword).
    ///
    /// # Returns
    ///
    /// - `Ok(value)`: The value read from the device register.
    /// - `Err(error)`: An error if the read operation failed.
    ///
    /// # Notes
    ///
    /// Implementations should respect the `width` parameter and only return
    /// data of the appropriate size. The returned value should be zero-extended
    /// if necessary.
    fn handle_read(&self, addr: R::Addr, width: AccessWidth) -> AxResult<usize>;

    /// Handles a write operation on the emulated device.
    ///
    /// # Arguments
    ///
    /// - `addr`: The address within the device's range being written.
    /// - `width`: The access width (byte, halfword, word, or doubleword).
    /// - `val`: The value to write to the device register.
    ///
    /// # Returns
    ///
    /// - `Ok(())`: The write operation completed successfully.
    /// - `Err(error)`: An error if the write operation failed.
    ///
    /// # Notes
    ///
    /// Implementations should only use the lower bits of `val` corresponding
    /// to the specified `width`.
    fn handle_write(&self, addr: R::Addr, width: AccessWidth, val: usize) -> AxResult;
}

/// Attempts to downcast a device to a specific type and apply a function to it.
///
/// This function is useful when you have a trait object (`Arc<dyn BaseDeviceOps<R>>`)
/// and need to access type-specific methods or data of the underlying concrete type.
///
/// # Type Parameters
///
/// - `T`: The concrete device type to downcast to. Must implement `BaseDeviceOps<R>`.
/// - `R`: The address range type.
/// - `U`: The return type of the mapping function.
/// - `F`: The function to apply if the downcast succeeds.
///
/// # Arguments
///
/// - `device`: A reference to the device trait object.
/// - `f`: A function to call with a reference to the concrete device type.
///
/// # Returns
///
/// - `Some(result)`: If the device is of type `T`, returns the result of `f`.
/// - `None`: If the device is not of type `T`.
///
/// # Example
///
/// ```rust,ignore
/// use axdevice_base::{BaseDeviceOps, map_device_of_type};
/// use alloc::sync::Arc;
///
/// struct UartDevice {
///     baud_rate: u32,
/// }
///
/// impl UartDevice {
///     fn get_baud_rate(&self) -> u32 {
///         self.baud_rate
///     }
/// }
///
/// // ... implement BaseDeviceOps for UartDevice ...
///
/// fn check_uart_config(device: &Arc<dyn BaseMmioDeviceOps>) {
///     if let Some(baud_rate) = map_device_of_type(device, |uart: &UartDevice| {
///         uart.get_baud_rate()
///     }) {
///         println!("UART baud rate: {}", baud_rate);
///     }
/// }
/// ```
#[deprecated(
    since = "0.5.0",
    note = "Use Device::as_any().downcast_ref() via MmioDeviceAdapter instead"
)]
pub fn map_device_of_type<T: BaseDeviceOps<R>, R: DeviceAddrRange, U, F: FnOnce(&T) -> U>(
    device: &Arc<dyn BaseDeviceOps<R>>,
    f: F,
) -> Option<U> {
    let any_arc: Arc<dyn Any> = device.clone();

    any_arc.downcast_ref::<T>().map(f)
}

// Trait aliases are limited yet: https://github.com/rust-lang/rfcs/pull/3437

/// Trait alias for MMIO (Memory-Mapped I/O) device operations.
///
/// This is a convenience alias for [`BaseDeviceOps`] with [`GuestPhysAddrRange`]
/// as the address range type. MMIO devices are the most common type of virtual
/// devices, where device registers are accessed through memory read/write operations.
///
/// # Supported Architectures
///
/// MMIO devices are supported on all architectures (x86_64, ARM, RISC-V).
pub trait BaseMmioDeviceOps = BaseDeviceOps<GuestPhysAddrRange>;

/// Trait alias for system register device operations.
///
/// This is a convenience alias for [`BaseDeviceOps`] with [`SysRegAddrRange`]
/// as the address range type. System register devices are typically used on
/// ARM architectures to emulate system registers accessed via MSR/MRS instructions.
///
/// # Supported Architectures
///
/// System register devices are primarily used on ARM/AArch64 architectures.
pub trait BaseSysRegDeviceOps = BaseDeviceOps<SysRegAddrRange>;

/// Trait alias for port I/O device operations.
///
/// This is a convenience alias for [`BaseDeviceOps`] with [`PortRange`]
/// as the address range type. Port I/O devices are used on x86 architectures
/// where device registers are accessed via IN/OUT instructions.
///
/// # Supported Architectures
///
/// Port I/O devices are only used on x86/x86_64 architectures.
pub trait BasePortDeviceOps = BaseDeviceOps<PortRange>;

// ---------------------------------------------------------------------------
// New unified device-registration types (device / interrupt framework refactoring)
// ---------------------------------------------------------------------------

/// Opaque identifier assigned to a device when it is registered into a
/// [`AxVmDevices`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DeviceId(u32);

impl DeviceId {
    /// Creates a new `DeviceId` from a raw `u32`.
    pub const fn new(id: u32) -> Self {
        Self(id)
    }

    /// Returns the raw `u32` value.
    pub const fn as_u32(self) -> u32 {
        self.0
    }
}

/// Target instruction-set architecture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arch {
    /// 64-bit ARM (AArch64).
    AArch64,
    /// 64-bit RISC-V.
    Riscv64,
    /// 64-bit x86 (AMD64 / Intel 64).
    X86_64,
    /// 64-bit LoongArch.
    LoongArch64,
}

/// Which vCPU(s) an interrupt should be delivered to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IrqTarget {
    /// A specific vCPU by its index.
    VCpu(usize),
    /// Broadcast to every vCPU in the VM.
    AllVCpus,
    /// Deliver to the vCPU currently running at the lowest priority
    /// (architecture-dependent).
    LowestPriority,
}

/// Trigger configuration for an interrupt line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IrqConfig {
    /// Edge-triggered interrupt.
    Edge,
    /// Level-triggered interrupt.
    Level,
}

/// The reason an IRQ resource registration was rejected.
#[derive(Debug, Clone)]
pub enum IrqConflictReason {
    /// The IRQ number exceeds the maximum supported value.
    OutOfRange {
        /// The requested IRQ line.
        irq: u32,
        /// The maximum valid IRQ line for this architecture.
        max: u32,
    },
    /// The IRQ line is already exclusively owned by another device.
    AlreadyExclusive {
        /// The conflicting IRQ line.
        irq: u32,
        /// The device that already owns this line.
        owner: DeviceId,
    },
    /// The trigger mode does not match the existing configuration on this
    /// line.
    TriggerMismatch {
        /// The IRQ line.
        irq: u32,
        /// The trigger mode the new device expects.
        expected: IrqConfig,
        /// The trigger mode already configured on this line.
        actual: IrqConfig,
    },
}

/// The kind of a resource a device requests.
///
/// Used in [`RegistryError::MissingRequiredResource`] to report which
/// required resource category was not declared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceKind {
    /// MMIO address range.
    Mmio,
    /// Port I/O range.
    Port,
    /// System register.
    SysReg,
    /// Interrupt line.
    Irq,
    /// MSI / MSI-X vector block.
    Msi,
}

/// A resource that a device declares it needs during registration.
///
/// The device manager uses this information for address-range conflict
/// detection, IRQ routing setup, and architecture-suitability checks.
#[derive(Debug, Clone)]
pub enum Resource {
    /// An MMIO address window.
    MmioRange {
        /// Start of the window (guest-physical address).
        base: u64,
        /// Size of the window in bytes.
        size: u64,
    },
    /// A Port I/O range (x86 only).
    PortRange {
        /// Start of the range.
        base: u16,
        /// Size of the range in bytes.
        size: u16,
    },
    /// System register range.
    SysReg {
        /// Register encoding range start (architecture-specific).
        addr: u32,
        /// Number of registers in the range.
        count: u32,
    },
    /// An interrupt line.
    Irq {
        /// IRQ number.
        line: u32,
        /// Which vCPU(s) the interrupt targets.
        target: IrqTarget,
    },
    /// A dynamic PCI BAR that will be programmed at runtime.
    PciBar {
        /// BAR index (0–5).
        bar_index: u8,
        /// Requested size in bytes.
        size: u64,
        /// `true` if this BAR requests PIO space; `false` for MMIO.
        is_pio: bool,
    },
    /// MSI / MSI-X capability with a requested number of vectors.
    MSI {
        /// Number of interrupt vectors requested.
        count: u32,
    },
    /// Marker indicating the device is capable of DMA (used for IOMMU /
    /// security checks). This does not consume an address range.
    DmaCapable,
}

/// Errors that can be returned when registering or unregistering a device.
#[derive(Debug, Clone)]
pub enum RegistryError {
    /// The given [`DeviceId`] does not refer to a currently-registered device.
    DeviceNotFound(DeviceId),
    /// Two devices claim overlapping address ranges.
    AddressConflict {
        /// The resource the new device is attempting to register.
        resource: Resource,
        /// The resource already held by an existing device.
        existing: Resource,
        /// The device that already owns the conflicting resource.
        existing_device: DeviceId,
    },
    /// The device requested a bus type that the current architecture does
    /// not support (e.g. Port I/O on AArch64).
    BusKindNotSupported {
        /// The unsupported bus kind.
        kind: BusKind,
        /// The current target architecture.
        arch: Arch,
    },
    /// An IRQ resource could not be allocated.
    IrqConflict {
        /// The IRQ line that caused the conflict.
        irq: u32,
        /// The reason for the conflict.
        reason: IrqConflictReason,
    },
    /// The device is not compatible with the current target architecture.
    ArchNotSupported {
        /// Human-readable device name (for diagnostics).
        device_name: String,
        /// The architecture(s) the device requires.
        required_arch: Arch,
        /// The architecture the hypervisor is currently built for.
        current_arch: Arch,
    },
    /// The device did not declare a resource that is mandatory for its
    /// device class.
    MissingRequiredResource {
        /// The device that is missing a resource.
        device: DeviceId,
        /// The kind of resource that is missing.
        missing: ResourceKind,
    },
}

/// The unified device trait.
///
/// Every emulated device (interrupt controller, UART, virtio-blk, …)
/// implements this trait.  The device manager calls [`resources`](Device::resources)
/// at registration time for conflict detection and [`handle`](Device::handle)
/// on the hot path whenever a vCPU exit is dispatched to this device.
///
/// # Downcasting
///
/// `Device` extends [`Any`](core::any::Any) so callers can downcast to a
/// concrete device type via [`as_any`](Device::as_any):
///
/// ```ignore
/// if let Some(vgic) = device.as_any().downcast_ref::<VGicD>() {
///     vgic.assign_irq(32, cpu_id, (0, 0, 0, cpu_id));
/// }
/// ```
pub trait Device: Send + Sync + Any {
    /// Returns a human-readable name for this device (used in logging and
    /// diagnostics).
    fn name(&self) -> &str;

    /// Returns the set of resources (MMIO windows, port ranges, IRQ lines,
    /// …) this device requires.
    fn resources(&self) -> Vec<Resource>;

    /// Handles a single bus access.
    ///
    /// This is the hot-path entry point called from [`BusRouter::dispatch`].
    fn handle(&self, access: &BusAccess) -> Result<BusResponse, DeviceError>;

    /// Returns a reference to `self` as `&dyn Any` for downcasting.
    fn as_any(&self) -> &dyn Any;

    /// Resets the device to its power-on state.
    #[allow(unused_variables)]
    fn reset(&mut self) -> Result<(), DeviceError> {
        Ok(())
    }

    /// Puts the device into a low-power or suspended state.
    #[allow(unused_variables)]
    fn suspend(&mut self) -> Result<(), DeviceError> {
        Ok(())
    }

    /// Restores the device from a suspended state.
    #[allow(unused_variables)]
    fn resume(&mut self) -> Result<(), DeviceError> {
        Ok(())
    }
}

/// Device registration interface — the build-time / management-path half of a
/// [`AxVmDevices`].
///
/// Used when constructing or reconfiguring a VM; not on the vCPU hot path.
pub trait DeviceRegistry {
    /// Registers a device, performing resource conflict detection and
    /// architecture-suitability checks.
    ///
    /// On success the device is assigned a unique [`DeviceId`] and inserted
    /// into the manager's lookup structures.
    fn register(&mut self, device: Arc<dyn Device>) -> Result<DeviceId, RegistryError>;

    /// Unregisters a previously registered device, removing its resources
    /// from all lookup structures and freeing its slot.
    fn unregister(&mut self, id: DeviceId) -> Result<(), RegistryError>;
}

/// Bus dispatch interface — the runtime hot-path half of a
/// [`AxVmDevices`].
///
/// Called on every vCPU exit that targets an emulated device (MMIO / Port /
/// SysReg).
pub trait BusRouter {
    /// Looks up the device responsible for `access` and forwards the access
    /// to it, returning the result.
    fn dispatch(&self, access: &BusAccess) -> Result<BusResponse, DeviceError>;

    /// Looks up the device responsible for `access` without handling the
    /// access.  The caller can then inspect the device or call
    /// [`Device::handle`] manually.
    fn lookup(&self, access: &BusAccess) -> Result<Arc<dyn Device>, DeviceError>;
}

// ---------------------------------------------------------------------------
// Sub-modules
// ---------------------------------------------------------------------------

mod adapter;
mod irq;

pub use adapter::{MmioDeviceAdapter, PortDeviceAdapter, SysRegDeviceAdapter};
pub use irq::{IrqLine, IrqSink};
