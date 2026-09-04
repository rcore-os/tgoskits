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
//! - [`DeviceAccess`]: Immutable metadata for one guest device access.
//! - [`DeviceContext`]: Access-scoped runtime capabilities passed to devices.
//! - [`Resource`]: Static device resource declarations used for registration
//!   validation and bus dispatch.
//! - [`VirtualInterruptController`], [`WiredIrqInput`], and [`IrqLine`]:
//!   architecture-neutral interrupt connections used by device factories.
//!
//! # Usage
//!
//! New emulated devices should implement [`Device`] directly and receive all
//! sensitive runtime abilities through [`DeviceContext`].
//!
//! ```rust,ignore
//! use axdevice_base::{
//!     AccessWidth, BusKind, Device, DeviceAccess, DeviceContext, DeviceError,
//!     DeviceVcpuId, Resource,
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
//!     fn read(
//!         &self,
//!         access: &DeviceAccess,
//!         _context: &mut dyn DeviceContext,
//!     ) -> Result<u64, DeviceError> {
//!         match access.bus() {
//!             BusKind::Mmio => Ok(0),
//!             _ => Err(DeviceError::OutOfRange {
//!                 addr: access.address(),
//!             }),
//!         }
//!     }
//!
//!     fn write(
//!         &self,
//!         access: &DeviceAccess,
//!         _value: u64,
//!         _context: &mut dyn DeviceContext,
//!     ) -> Result<(), DeviceError> {
//!         match access.bus() {
//!             BusKind::Mmio => Ok(()),
//!             _ => Err(DeviceError::OutOfRange {
//!                 addr: access.address(),
//!             }),
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
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

pub use axvm_types::{GuestPhysAddr, GuestPhysAddrRange, InterruptTriggerMode, IrqLineId};

pub use crate::device::{
    AccessWidth, BusKind, DeviceAccess, DeviceAddr, DeviceAddrRange, DeviceError, DeviceResult,
    Port, PortRange, SysRegAddr, SysRegAddrRange,
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

/// VM-local identity of the vCPU that issued one trapped device access.
///
/// This value describes the architectural accessor, not the physical CPU that
/// happens to execute the device callback. It remains valid when exit handling
/// is preempted or migrates between host CPUs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DeviceVcpuId(usize);

impl DeviceVcpuId {
    /// Creates a device-access vCPU identifier from its VM-local value.
    pub const fn new(id: usize) -> Self {
        Self(id)
    }

    /// Returns the VM-local numeric identifier.
    pub const fn as_usize(self) -> usize {
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
/// at registration time for conflict detection and [`read`](Device::read) or
/// [`write`](Device::write) on the hot path whenever a vCPU exit is dispatched
/// to this device.
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

    /// Handles one guest read with runtime-scoped device capabilities.
    fn read(&self, access: &DeviceAccess, context: &mut dyn DeviceContext) -> DeviceResult<u64>;

    /// Handles one guest write with runtime-scoped device capabilities.
    fn write(
        &self,
        access: &DeviceAccess,
        value: u64,
        context: &mut dyn DeviceContext,
    ) -> DeviceResult;
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

/// Binding generation for one routed-device grant.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RoutedBindingGeneration(u64);

impl RoutedBindingGeneration {
    /// Creates a binding generation value.
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the numeric generation value for diagnostics and comparisons.
    pub const fn value(self) -> u64 {
        self.0
    }
}

/// Admission epoch for one routed-device grant.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RoutedAdmissionEpoch(u64);

impl RoutedAdmissionEpoch {
    /// Creates an admission epoch value.
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the numeric epoch value for diagnostics and comparisons.
    pub const fn value(self) -> u64 {
        self.0
    }
}

/// Runtime-created capability for entering one routed device context.
///
/// The constructor only creates an inert handle. The device runtime gives a
/// handle authority by registering the exact token for one final device,
/// binding generation, and admission epoch. The DMA flag is a root-side
/// snapshot: it never replaces the endpoint's separately registered
/// [`DmaGrant`].
struct RoutedGrantAdmission {
    open: AtomicBool,
    epoch: AtomicU64,
}

impl RoutedGrantAdmission {
    #[cfg(feature = "runtime-internal")]
    fn new(epoch: RoutedAdmissionEpoch) -> Self {
        Self {
            open: AtomicBool::new(true),
            epoch: AtomicU64::new(epoch.value()),
        }
    }

    fn is_open_at(&self, epoch: RoutedAdmissionEpoch) -> bool {
        self.epoch.load(Ordering::Acquire) == epoch.value() && self.open.load(Ordering::Acquire)
    }

    #[cfg(feature = "runtime-internal")]
    fn close(&self) {
        self.open.store(false, Ordering::Release);
    }

    #[cfg(feature = "runtime-internal")]
    fn reopen(&self, epoch: RoutedAdmissionEpoch) {
        self.epoch.store(epoch.value(), Ordering::Release);
        self.open.store(true, Ordering::Release);
    }
}

/// Lifetime state for a grant that has already passed routed admission.
///
/// This state is deliberately separate from [`RoutedGrantAdmission`]. The
/// latter controls whether a new callback may be admitted; this state keeps
/// one callback admitted until its runtime-owned scope guard is dropped.
struct ScopedRoutedGrantAdmission {
    active: AtomicBool,
}

impl ScopedRoutedGrantAdmission {
    #[cfg(feature = "runtime-internal")]
    fn new() -> Self {
        Self {
            active: AtomicBool::new(true),
        }
    }

    fn is_active(&self) -> bool {
        // Acquire observes the Release performed by the scope guard's Drop.
        self.active.load(Ordering::Acquire)
    }

    #[cfg(feature = "runtime-internal")]
    fn close(&self) {
        self.active.store(false, Ordering::Release);
    }
}

/// Runtime-owned guard that keeps an admitted routed grant valid.
///
/// This type is only constructed by the device runtime after a route
/// admission succeeds. Cloning the associated grant does not extend this
/// guard's lifetime: every clone observes the same active bit.
#[cfg(feature = "runtime-internal")]
#[doc(hidden)]
#[must_use = "the scope guard keeps an admitted routed grant valid"]
pub struct RoutedGrantScope {
    admission: Arc<ScopedRoutedGrantAdmission>,
}

#[cfg(feature = "runtime-internal")]
impl Drop for RoutedGrantScope {
    fn drop(&mut self) {
        self.admission.close();
    }
}

/// An inert or scope-bound handle for temporarily entering one runtime-routed
/// device.
#[derive(Clone)]
pub struct RoutedDeviceGrant {
    device_id: DeviceId,
    binding_generation: RoutedBindingGeneration,
    admission_epoch: RoutedAdmissionEpoch,
    dma_enabled: bool,
    token: Arc<()>,
    admission: Arc<RoutedGrantAdmission>,
    scoped_admission: Option<Arc<ScopedRoutedGrantAdmission>>,
}

impl RoutedDeviceGrant {
    /// Creates an inert routed-device handle for a binding snapshot.
    #[cfg(feature = "runtime-internal")]
    #[doc(hidden)]
    pub fn new(
        device_id: DeviceId,
        binding_generation: RoutedBindingGeneration,
        admission_epoch: RoutedAdmissionEpoch,
        dma_enabled: bool,
    ) -> Self {
        Self {
            device_id,
            binding_generation,
            admission_epoch,
            dma_enabled,
            token: Arc::new(()),
            admission: Arc::new(RoutedGrantAdmission::new(admission_epoch)),
            scoped_admission: None,
        }
    }

    /// Returns the final device identity selected by this route.
    pub const fn device_id(&self) -> DeviceId {
        self.device_id
    }

    /// Returns the binding generation selected by this route.
    pub const fn binding_generation(&self) -> RoutedBindingGeneration {
        self.binding_generation
    }

    /// Returns the admission epoch selected by this route.
    pub const fn admission_epoch(&self) -> RoutedAdmissionEpoch {
        self.admission_epoch
    }

    /// Returns the root's bus-master-enable snapshot.
    pub const fn dma_enabled(&self) -> bool {
        self.dma_enabled
    }

    /// Returns whether two handles carry the same routing authority.
    pub fn same_token(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.token, &other.token)
    }

    /// Returns whether this snapshot may enter a routed device context.
    ///
    /// Ordinary snapshots require their shared admission epoch to remain
    /// open. A snapshot produced by the runtime's admission conversion
    /// instead remains valid until its scope guard is dropped, so closing the
    /// shared admission does not revoke an already admitted callback.
    pub fn admission_is_open(&self) -> bool {
        self.scoped_admission.as_ref().map_or_else(
            || self.admission.is_open_at(self.admission_epoch),
            |admission| admission.is_active(),
        )
    }

    /// Converts an already validated, currently admitted grant into a
    /// scope-bound grant.
    ///
    /// The caller must retain the returned guard for the full callback
    /// lifetime. Dropping it invalidates this grant and all of its clones.
    #[cfg(feature = "runtime-internal")]
    #[doc(hidden)]
    pub fn admit(mut self) -> Option<(Self, RoutedGrantScope)> {
        if self.scoped_admission.is_some() || !self.admission.is_open_at(self.admission_epoch) {
            return None;
        }
        let admission = Arc::new(ScopedRoutedGrantAdmission::new());
        self.scoped_admission = Some(admission.clone());
        Some((self, RoutedGrantScope { admission }))
    }

    /// Closes validation for this grant's current epoch.
    #[cfg(feature = "runtime-internal")]
    #[doc(hidden)]
    pub fn close_admission(&self) {
        self.admission.close();
    }

    /// Creates the next admission-epoch snapshot without changing binding
    /// generation or routing identity.
    #[cfg(feature = "runtime-internal")]
    #[doc(hidden)]
    pub fn with_admission_epoch(&self, admission_epoch: RoutedAdmissionEpoch) -> Self {
        Self {
            device_id: self.device_id,
            binding_generation: self.binding_generation,
            admission_epoch,
            dma_enabled: self.dma_enabled,
            token: self.token.clone(),
            admission: self.admission.clone(),
            scoped_admission: None,
        }
    }

    /// Reopens this grant's shared admission at a fresh epoch.
    #[cfg(feature = "runtime-internal")]
    #[doc(hidden)]
    pub fn reopen_admission(&self, admission_epoch: RoutedAdmissionEpoch) {
        self.admission.reopen(admission_epoch);
    }

    /// Copies this route with a freshly captured bus-master-enable snapshot.
    #[cfg(feature = "runtime-internal")]
    #[doc(hidden)]
    pub fn with_dma_enabled(&self, dma_enabled: bool) -> Self {
        Self {
            device_id: self.device_id,
            binding_generation: self.binding_generation,
            admission_epoch: self.admission_epoch,
            dma_enabled,
            token: self.token.clone(),
            admission: self.admission.clone(),
            scoped_admission: self.scoped_admission.clone(),
        }
    }
}

/// Guest-memory operations made available to a device runtime.
///
/// This boundary deliberately carries no device identity or grant. The
/// runtime validates those before delegating to this VM-owned memory port.
pub trait GuestMemoryAccess {
    /// Reads bytes from guest physical memory.
    fn read(&mut self, addr: GuestPhysAddr, data: &mut [u8]) -> DeviceResult;

    /// Writes bytes to guest physical memory.
    fn write(&mut self, addr: GuestPhysAddr, data: &[u8]) -> DeviceResult;
}

/// Runtime capability context scoped to one device callback.
///
/// The device runtime creates this context immediately before calling
/// [`Device::read`] or [`Device::write`] and drops it before returning to the
/// architecture exit handler. Sensitive abilities such as guest-memory DMA,
/// timer scheduling, vCPU wake, and VM stop requests are denied by default and
/// become available only when the current device presents the matching
/// registration-time grant.
pub trait DeviceContext {
    /// Returns the identity of the device currently handling this access.
    fn device_id(&self) -> DeviceId;

    /// Enters a runtime-created routed device context.
    ///
    /// The default implementation is deliberately denied. Only the sealed
    /// device runtime can validate and materialize a routed context.
    fn with_routed_device(
        &mut self,
        _grant: &RoutedDeviceGrant,
        _callback: &mut dyn FnMut(&mut dyn DeviceContext) -> DeviceResult,
    ) -> DeviceResult {
        Err(DeviceError::Unsupported {
            operation: "enter routed device context",
            detail: "this device access has no routed-device grant".into(),
        })
    }

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

/// A no-permission device context for tests and adapter-only callers.
pub struct NoopDeviceContext {
    device_id: DeviceId,
}

impl NoopDeviceContext {
    /// Creates a no-permission context for `device_id`.
    pub const fn new(device_id: DeviceId) -> Self {
        Self { device_id }
    }
}

impl DeviceContext for NoopDeviceContext {
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

// ---------------------------------------------------------------------------
// Sub-modules
// ---------------------------------------------------------------------------

mod interrupt;

pub use interrupt::*;
