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
//! - [`EmuDeviceType`]: Runtime classification for emulator devices.
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
//! use axdevice_base::DeviceResult;
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
//!     fn handle_read(&self, addr: GuestPhysAddr, width: AccessWidth) -> DeviceResult<usize> {
//!         // Handle read operation
//!         Ok(0)
//!     }
//!
//!     fn handle_write(&self, addr: GuestPhysAddr, width: AccessWidth, val: usize) -> DeviceResult {
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

use alloc::{string::String, sync::Arc};
use core::any::Any;

pub use axvm_types::{
    EmulatedDeviceType as EmuDeviceType, GuestPhysAddr, GuestPhysAddrRange, InterruptTriggerMode,
    IrqLineId,
};

pub use crate::device::{
    AccessWidth, BusAccess, BusKind, BusResponse, DeviceAddr, DeviceAddrRange, DeviceError,
    DeviceResult, Port, PortRange, SysRegAddr, SysRegAddrRange,
};

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
    fn handle_read(&self, addr: R::Addr, width: AccessWidth) -> DeviceResult<usize>;

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
    fn handle_write(&self, addr: R::Addr, width: AccessWidth, val: usize) -> DeviceResult;
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
/// an AxVM device registry.
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

/// A resource that a device declares it needs during registration.
///
/// The device manager uses this information for address-range conflict
/// detection and architecture-suitability checks.
#[derive(Debug, Clone, Eq, PartialEq)]
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
    /// A planner-authorized wired interrupt-controller endpoint.
    ///
    /// Runtime devices cannot register this resource directly. The device
    /// build transaction derives it from an opaque interrupt claim and adds
    /// it to the bundle-level endpoint inventory.
    WiredIrq {
        /// Controller owning the input.
        controller: InterruptControllerId,
        /// Controller-local input number.
        input: ControllerInputId,
        /// Electrical trigger semantics.
        trigger: InterruptTriggerMode,
        /// Whether independently owned sources may share this input.
        sharing: InterruptSharing,
    },
    /// A planner-authorized message-signaled interrupt endpoint.
    MessageInterrupt {
        /// Controller receiving the message.
        controller: InterruptControllerId,
        /// Controller-local MSI device identity.
        device: MsiDeviceId,
        /// Device-local event identity.
        event: MsiEventId,
    },
}

/// VM-global identity of an interrupt endpoint.
///
/// Wired input numbers are controller-local, so the controller identifier is
/// part of the key. Message-signaled events are identified by the controller,
/// device, and event tuple.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum InterruptEndpointKey {
    /// One wired input on an interrupt controller.
    Wired {
        /// Controller that owns the input namespace.
        controller: InterruptControllerId,
        /// Input number within `controller`.
        input: ControllerInputId,
    },
    /// One message-signaled event accepted by an interrupt controller.
    Message {
        /// Controller that accepts the message.
        controller: InterruptControllerId,
        /// Controller-local message device identifier.
        device: MsiDeviceId,
        /// Event identifier within `device`.
        event: MsiEventId,
    },
}

impl Resource {
    /// Returns the VM-global endpoint key for an interrupt resource.
    pub const fn interrupt_endpoint_key(&self) -> Option<InterruptEndpointKey> {
        match *self {
            Self::WiredIrq {
                controller, input, ..
            } => Some(InterruptEndpointKey::Wired { controller, input }),
            Self::MessageInterrupt {
                controller,
                device,
                event,
            } => Some(InterruptEndpointKey::Message {
                controller,
                device,
                event,
            }),
            _ => None,
        }
    }

    /// Returns whether two interrupt resources make incompatible ownership
    /// claims on the same VM-global endpoint.
    pub fn interrupt_endpoint_conflicts_with(&self, other: &Self) -> bool {
        let Some(key) = self.interrupt_endpoint_key() else {
            return false;
        };
        if other.interrupt_endpoint_key() != Some(key) {
            return false;
        }
        !matches!(
            (self, other),
            (
                Self::WiredIrq {
                    trigger,
                    sharing: InterruptSharing::Shared,
                    ..
                },
                Self::WiredIrq {
                    trigger: other_trigger,
                    sharing: InterruptSharing::Shared,
                    ..
                },
            ) if trigger == other_trigger
        )
    }
}

/// The reason a resource was rejected as structurally invalid during
/// validation.
#[derive(Debug, Clone, Eq, PartialEq, thiserror::Error)]
pub enum InvalidResourceReason {
    /// The resource has a size or count of zero.
    #[error("resource size or count is zero")]
    ZeroSized,
    /// The resource's end address overflows the address space.
    #[error("resource end address overflows")]
    AddressOverflow,
    /// The resource extends past the valid bus address range.
    #[error("resource extends beyond the bus address range")]
    OutOfBusRange,
    /// The bus kind of the resource is not supported on the current
    /// architecture.
    #[error("resource bus is unsupported on this architecture")]
    UnsupportedOnArchitecture,
    /// The device declared multiple resources of the same bus kind whose
    /// address ranges overlap each other, which would corrupt the
    /// dispatch index.
    #[error("device resources overlap")]
    OverlappingResources,
    /// An interrupt endpoint was declared directly by a device instead of
    /// being backed by a planner-issued claim.
    #[error("interrupt endpoint is not backed by a planner claim")]
    UnbackedInterruptEndpoint,
}

/// Errors that can be returned when registering a device.
#[derive(Debug, Clone, Eq, PartialEq, thiserror::Error)]
pub enum RegistryError {
    /// The device declared a resource that is structurally invalid.
    #[error("invalid device resource {resource:?}: {reason}")]
    InvalidResource {
        /// The invalid resource.
        resource: Resource,
        /// Why the resource was rejected.
        reason: InvalidResourceReason,
    },
    /// Two devices claim overlapping address ranges.
    #[error(
        "device resource {resource:?} conflicts with {existing:?} owned by device \
         {existing_device:?}"
    )]
    AddressConflict {
        /// The resource the new device is attempting to register.
        resource: Resource,
        /// The resource already held by an existing device.
        existing: Resource,
        /// The device that already owns the conflicting resource.
        existing_device: DeviceId,
    },
    /// Two registered bundles claim incompatible ownership of one interrupt
    /// controller input.
    #[error(
        "interrupt resource {resource:?} conflicts with {existing:?} owned by device \
         {existing_device:?}"
    )]
    InterruptEndpointConflict {
        /// The endpoint requested by the new bundle.
        resource: Resource,
        /// The endpoint ownership already registered.
        existing: Resource,
        /// First device in the bundle that owns the existing endpoint.
        existing_device: DeviceId,
    },
    /// The device requested a bus type that the current architecture does
    /// not support (e.g. Port I/O on AArch64).
    #[error("device bus {kind:?} is unsupported on {arch:?}")]
    BusKindNotSupported {
        /// The unsupported bus kind.
        kind: BusKind,
        /// The current target architecture.
        arch: Arch,
    },
    /// The device is not compatible with the current target architecture.
    #[error(
        "device {device_name} requires {required_arch:?}, but the current architecture is \
         {current_arch:?}"
    )]
    ArchNotSupported {
        /// Human-readable device name (for diagnostics).
        device_name: String,
        /// The architecture(s) the device requires.
        required_arch: Arch,
        /// The architecture the hypervisor is currently built for.
        current_arch: Arch,
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
/// `Device` extends [`Any`] so callers can downcast to a
/// concrete device type via [`as_any`](Device::as_any). Downcasting is only
/// intended for device-specific data-plane operations; interrupt-controller
/// capabilities are registered separately and devices connect through owned
/// interrupt endpoints.
///
/// ```ignore
/// if let Some(console) = device.as_any().downcast_ref::<GuestConsole>() {
///     console.flush_output()?;
/// }
/// ```
pub trait Device: Send + Sync + Any {
    /// Returns a human-readable name for this device (used in logging and
    /// diagnostics).
    fn name(&self) -> &str;

    /// Returns the resources (MMIO windows, port ranges, system registers)
    /// this device requires.
    ///
    /// The returned slice is a stable snapshot computed at device construction
    /// time. Callers may read it on both the registration path and the hot
    /// path without allocation.
    fn resources(&self) -> &[Resource];

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
/// an AxVM device registry.
///
/// Used when constructing or reconfiguring a VM; not on the vCPU hot path.
pub trait DeviceRegistry {
    /// Registers a device, performing resource conflict detection and
    /// architecture-suitability checks.
    ///
    /// On success the device is assigned a unique [`DeviceId`] and inserted
    /// into the manager's lookup structures.
    fn register(&mut self, device: Arc<dyn Device>) -> Result<DeviceId, RegistryError>;
}

/// Bus dispatch interface — the runtime hot-path half of a
/// an AxVM device registry.
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
mod interrupt;

pub use adapter::{MmioDeviceAdapter, PortDeviceAdapter, SysRegDeviceAdapter};
pub use interrupt::*;
