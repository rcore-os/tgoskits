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
//! - [`Device`]: The unified V3 device trait used by the runtime hot path.
//! - [`DeviceAccess`]: The access-scoped capability context passed to devices.
//! - [`Resource`]: Static device resource declarations used for registration
//!   validation and bus dispatch.
//! - [`VirtualInterruptController`], [`WiredIrqInput`], and [`IrqLine`]:
//!   architecture-neutral interrupt connections used by device factories.
//!
//! # Usage
//!
//! New emulated devices should implement [`Device`] directly and receive all
//! sensitive runtime abilities through [`DeviceAccess`].
//!
//! ```rust,ignore
//! use axdevice_base::{
//!     AccessWidth, BusAccess, BusKind, BusResponse, Device, DeviceAccess,
//!     DeviceError, Resource,
//! };
//!
//! struct MyDevice {
//!     base_addr: usize,
//!     size: usize,
//!     resources: [Resource; 1],
//! }
//!
//! impl Device for MyDevice {
//!     fn name(&self) -> &str {
//!         "my-device"
//!     }
//!
//!     fn resources(&self) -> &[Resource] {
//!         &self.resources
//!     }
//!
//!     fn access(
//!         &self,
//!         access: &BusAccess,
//!         context: &mut dyn DeviceAccess,
//!     ) -> Result<BusResponse, DeviceError> {
//!         match (access.kind, access.is_read) {
//!             (BusKind::Mmio, true) => Ok(BusResponse::Read { value: 0 }),
//!             (BusKind::Mmio, false) => Ok(BusResponse::Write),
//!             _ => Err(DeviceError::OutOfRange { addr: access.addr }),
//!         }
//!     }
//! }
//! ```
//!
//! # Feature Flags
//!
//! This crate currently has no optional feature flags. All functionality is available
//! by default.

#![no_std]
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

pub use axvm_types::{GuestPhysAddr, GuestPhysAddrRange, InterruptTriggerMode, IrqLineId};

pub use crate::device::{
    AccessWidth, BusAccess, BusKind, BusResponse, DeviceAddr, DeviceAddrRange, DeviceError,
    DeviceResult, Port, PortRange, SysRegAddr, SysRegAddrRange,
};

// ---------------------------------------------------------------------------
// New unified device-registration types (device / interrupt framework refactoring)
// ---------------------------------------------------------------------------

/// Opaque identifier assigned to a device when it is registered into a
/// [`DeviceRuntime`].
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
    /// An exclusive IRQ line connected to the VM's interrupt fabric.
    ///
    /// The `line` is an architecture-neutral identifier on the virtual
    /// interrupt controller input side (e.g. GSI, INTID, or PLIC source).
    /// It is not a host `IrqId`, CPU trap vector, or physical IRQ.
    ///
    /// This stage only supports exclusive declaration. Sharing policy
    /// will be added when a concrete device needs it.
    IrqLine {
        /// The interrupt line number.
        line: u32,
        /// The trigger mode configured for this line.
        trigger: InterruptTriggerMode,
    },
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
    /// The device declared the same IRQ line more than once.
    #[error("duplicate IRQ line {line}")]
    DuplicateIrqLine {
        /// The duplicated line number.
        line: u32,
    },
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
    /// Two devices claim the same IRQ line.
    #[error("IRQ line {line} conflicts with device {existing_device:?}")]
    IrqLineConflict {
        /// The IRQ line the new device is attempting to register.
        line: u32,
        /// The device that already owns the conflicting IRQ line.
        existing_device: DeviceId,
    },
    /// The registry state does not allow the requested operation.
    #[error("invalid device registry state for {operation}: {detail}")]
    InvalidState {
        /// The operation rejected by the current state.
        operation: &'static str,
        /// Diagnostic detail describing the current state.
        detail: String,
    },
}

/// The unified device trait.
///
/// Every emulated device (interrupt controller, UART, virtio-blk, …)
/// implements this trait.  The device manager calls [`resources`](Device::resources)
/// at registration time for conflict detection and [`access`](Device::access)
/// on the hot path whenever a vCPU exit is dispatched to this device.
///
/// Concrete collaboration between devices and architecture code should be
/// exposed through typed services registered with the VM device runtime, not
/// through production downcasts from `Arc<dyn Device>`.
pub trait Device: Send + Sync {
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

    /// Handles a single bus access with runtime-scoped device context.
    fn access(
        &self,
        access: &BusAccess,
        context: &mut dyn DeviceAccess,
    ) -> Result<BusResponse, DeviceError>;
}

macro_rules! define_grant {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone)]
        pub struct $name {
            token: Arc<()>,
        }

        impl $name {
            /// Creates a grant token that has no authority until a
            /// [`DeviceRuntime`](crate::DeviceRegistry) records it for a
            /// specific device during transactional registration.
            pub fn new() -> Self {
                Self { token: Arc::new(()) }
            }

            /// Returns whether two handles refer to the same grant token.
            pub fn same_token(&self, other: &Self) -> bool {
                Arc::ptr_eq(&self.token, &other.token)
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }
    };
}

define_grant!(
    /// Permission token for access-scoped guest-memory DMA.
    DmaGrant
);
define_grant!(
    /// Permission token for virtual timer scheduling.
    TimerGrant
);
define_grant!(
    /// Permission token for waking a vCPU from a device.
    WakeGrant
);
define_grant!(
    /// Permission token for requesting VM stop/suspend style actions.
    StopGrant
);

/// Context scoped to one device bus access.
///
/// A [`BusRouter`] creates this context immediately before calling
/// [`Device::access`] and drops it before returning to the architecture exit
/// handler. Sensitive abilities such as guest-memory DMA, timer scheduling,
/// vCPU wake, and VM stop requests are denied by default and become available
/// only when the current device presents the matching registration-time grant.
pub trait DeviceAccess {
    /// Returns the identity of the device currently handling this access.
    fn device_id(&self) -> DeviceId;

    /// Reads guest memory on behalf of the currently dispatched device.
    ///
    /// This capability is valid only for this access and is denied by default.
    fn read_guest_memory(
        &mut self,
        _grant: &DmaGrant,
        _addr: GuestPhysAddr,
        _data: &mut [u8],
    ) -> DeviceResult {
        Err(DeviceError::Unsupported {
            operation: "read guest memory from device access",
            detail: "this bus access has no DMA memory grant".into(),
        })
    }

    /// Writes guest memory on behalf of the currently dispatched device.
    fn write_guest_memory(
        &mut self,
        _grant: &DmaGrant,
        _addr: GuestPhysAddr,
        _data: &[u8],
    ) -> DeviceResult {
        Err(DeviceError::Unsupported {
            operation: "write guest memory from device access",
            detail: "this bus access has no DMA memory grant".into(),
        })
    }

    /// Schedules a virtual timer on behalf of the current device.
    fn schedule_timer(&mut self, _grant: &TimerGrant, _deadline_ns: u64) -> DeviceResult {
        Err(DeviceError::Unsupported {
            operation: "schedule timer from device access",
            detail: "this bus access has no timer grant".into(),
        })
    }

    /// Wakes a vCPU on behalf of the current device.
    fn wake_vcpu(&mut self, _grant: &WakeGrant, _vcpu_id: usize) -> DeviceResult {
        Err(DeviceError::Unsupported {
            operation: "wake vCPU from device access",
            detail: "this bus access has no wake grant".into(),
        })
    }

    /// Requests that the VM stops because of a device-visible condition.
    fn request_vm_stop(&mut self, _grant: &StopGrant, _reason: &str) -> DeviceResult {
        Err(DeviceError::Unsupported {
            operation: "request VM stop from device access",
            detail: "this bus access has no stop grant".into(),
        })
    }
}

/// A no-permission access context for tests and adapter-only callers.
pub struct NoopDeviceAccess {
    device_id: DeviceId,
}

impl NoopDeviceAccess {
    /// Creates a no-permission context for `device_id`.
    pub const fn new(device_id: DeviceId) -> Self {
        Self { device_id }
    }
}

impl DeviceAccess for NoopDeviceAccess {
    fn device_id(&self) -> DeviceId {
        self.device_id
    }
}

/// Device registration interface — the build-time / management-path half of a
/// [`DeviceRuntime`].
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
/// [`DeviceRuntime`].
///
/// Called on every vCPU exit that targets an emulated device (MMIO / Port /
/// SysReg).
pub trait BusRouter {
    /// Looks up the device responsible for `access` and forwards the access
    /// to it, returning the result.
    fn dispatch(&self, access: &BusAccess) -> Result<BusResponse, DeviceError>;

    /// Looks up the device responsible for `access` without handling the access.
    fn lookup(&self, access: &BusAccess) -> Result<Arc<dyn Device>, DeviceError>;
}

// ---------------------------------------------------------------------------
// Sub-modules
// ---------------------------------------------------------------------------

mod interrupt;

pub use interrupt::*;
