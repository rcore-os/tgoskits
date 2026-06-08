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

//! Unified device & bus abstraction layer for AxVisor.
//!
//! Inspired by crosvm's `BusDevice` + `Bus`, Firecracker's `MMIODeviceManager`,
//! and ACRN's emulation handler tables. Provides a strongly-typed, extensible
//! framework for device emulation across all bus types.

use alloc::boxed::Box;
use alloc::string::String;
//use alloc::sync::Arc;
//use alloc::vec::Vec;
use core::any::Any;
use core::fmt::Display;
use core::ops::Range;

use crate::irq::InterruptControllerOps;

// ============================================================
// 1. Value types
// ============================================================

/// Globally unique device identifier within a VM.
///
/// Allocation strategy: each VM-local `DeviceRegistry` assigns monotonically
/// increasing IDs via `slotmap` at registration time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DeviceId(pub u64);

impl DeviceId {
    pub const fn from_u64(v: u64) -> Self {
        Self(v)
    }
}

/// An interrupt line number in the guest's interrupt controller space.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct IrqLine(pub u32);

/// Where an interrupt line is routed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IrqTarget {
    /// Specific vCPU (by vCPU ID).
    Cpu(usize),
    /// A named interrupt controller device (by its DeviceId).
    Controller(DeviceId),
    /// Platform-global interrupt (routed by the platform's interrupt controller).
    Global,
}

/// Resource types a device can claim.
///
/// Used for address-space registration, conflict detection, and FDT generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resource {
    /// Memory-mapped I/O region (MMIO).
    Mmio(Range<u64>),
    /// Port I/O region (PIO / x86 I/O space).
    Pio(Range<u16>),
    /// System register range (ARM mrs/msr, x86 MSR, RISC-V CSR).
    SysReg(Range<u64>),
    /// Interrupt line. Routing is configured in `IrqRoutingTable`.
    Irq(IrqLine),
}

/// Supported bus types. Extensible: new variants (e.g., `PciConfig`, `Imsic`)
/// can be added without breaking existing dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BusKind {
    Mmio,
    Pio,
    SysReg,
}

/// Width of a single bus access (guest-driven).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessWidth {
    /// 8-bit access (byte)
    U8 = 1,
    /// 16-bit access (half-word)
    U16 = 2,
    /// 32-bit access (word)
    U32 = 4,
    /// 64-bit access (double-word)
    U64 = 8,
}

impl From<axaddrspace::device::AccessWidth> for AccessWidth {
    fn from(w: axaddrspace::device::AccessWidth) -> Self {
        match w {
            axaddrspace::device::AccessWidth::Byte => Self::U8,
            axaddrspace::device::AccessWidth::Word => Self::U16,
            axaddrspace::device::AccessWidth::Dword => Self::U32,
            axaddrspace::device::AccessWidth::Qword => Self::U64,
        }
    }
}

impl From<AccessWidth> for axaddrspace::device::AccessWidth {
    fn from(w: AccessWidth) -> Self {
        match w {
            AccessWidth::U8 => Self::Byte,
            AccessWidth::U16 => Self::Word,
            AccessWidth::U32 => Self::Dword,
            AccessWidth::U64 => Self::Qword,
        }
    }
}

// ============================================================
// 2. Bus access / response protocol
// ============================================================

/// A decoded access from the guest.
#[derive(Debug, Clone)]
pub enum BusAccess {
    /// Read from the given address with the given width.
    Read {
        /// Guest physical or port address.
        addr: u64,
        /// Width of the access.
        width: AccessWidth,
    },
    /// Write the given value to the given address with the given width.
    Write {
        /// Guest physical or port address.
        addr: u64,
        /// Width of the access.
        width: AccessWidth,
        /// Value to write.
        val: u64,
    },
}

impl BusAccess {
    pub fn is_read(&self) -> bool {
        matches!(self, Self::Read { .. })
    }

    pub fn addr(&self) -> u64 {
        match self {
            Self::Read { addr, .. } | Self::Write { addr, .. } => *addr,
        }
    }

    pub fn width(&self) -> AccessWidth {
        match self {
            Self::Read { width, .. } | Self::Write { width, .. } => *width,
        }
    }
}

/// The result of routing a bus access to a device.
#[derive(Debug, Clone)]
pub enum BusResponse {
    /// Access completed successfully, optionally returning data (for reads).
    Success(Option<u64>),
    /// No device claimed the address on the given bus.
    NoDevice { bus: BusKind, addr: u64 },
    /// The access width is not supported by the device at this address.
    InvalidWidth { bus: BusKind, addr: u64, width: AccessWidth },
    /// Attempted write to a read-only register.
    ReadOnly { bus: BusKind, addr: u64 },
    /// Device-specific error (e.g., legacy backend failure).
    DeviceError { bus: BusKind, addr: u64, msg: &'static str },
}

impl BusResponse {
    pub fn is_success(&self) -> bool {
        matches!(self, Self::Success(_))
    }

    pub fn value(&self) -> Option<u64> {
        match self {
            Self::Success(v) => *v,
            _ => None,
        }
    }
}

impl core::fmt::Display for BusResponse {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Success(Some(v)) => write!(f, "ok({v:#x})"),
            Self::Success(None) => write!(f, "ok"),
            Self::NoDevice { bus, addr } => write!(f, "no device on {bus:?} @ {addr:#x}"),
            Self::InvalidWidth { bus, addr, width } => {
                write!(f, "invalid width {width:?} on {bus:?} @ {addr:#x}")
            }
            Self::ReadOnly { bus, addr } => write!(f, "read-only {bus:?} @ {addr:#x}"),
            Self::DeviceError { bus, addr, msg } => {
                write!(f, "device error on {bus:?} @ {addr:#x}: {msg}")
            }
        }
    }
}

// ============================================================
// 3. Error type
// ============================================================

/// Errors originating from device or bus operations.
#[derive(Debug)]
pub enum DeviceError {
    /// Address range overlaps with an already-registered device.
    AddressConflict(Resource),
    /// The resources supplied are malformed (zero-length, misaligned, etc.).
    InvalidResource,
    /// Backend-specific failure (delegated to device driver).
    BackendError(String),
    /// A device with the same identity/type already exists.
    AlreadyExists,
    /// The requested device/address was not found.
    NotFound,
}

impl Display for DeviceError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::AddressConflict(r) => write!(f, "address conflict on resource {r:?}"),
            Self::InvalidResource => write!(f, "invalid device resource"),
            Self::BackendError(msg) => write!(f, "backend error: {msg}"),
            Self::AlreadyExists => write!(f, "device already exists"),
            Self::NotFound => write!(f, "device not found"),
        }
    }
}

/// Convenience alias for bus/device operations.
pub type Result<T> = core::result::Result<T, DeviceError>;

// ============================================================
// 5. Core device trait
// ============================================================

/// The single trait that **every** emulated device in AxVisor exposes to the VMM.
///
/// ```text
/// ┌─────────────────────────────────────────────┐
/// │              VirtualDevice                   │
/// │  + id() → DeviceId                          │
/// │  + name() → &str                            │
/// │  + resources() → &[Resource]                │
/// │  + handle_access(BusKind, &BusAccess) ─→ BusResponse │
/// │  + as_interrupt_controller() → Option<…>    │
/// │  + as_any() → &dyn Any                      │
/// └─────────────────────────────────────────────┘
/// ```
pub trait VirtualDevice: Send + Sync + core::fmt::Debug {
    /// Unique identifier assigned at registration time.
    fn id(&self) -> DeviceId;

    /// Human-readable name (for debug / logs / FDT generation).
    fn name(&self) -> &str;

    /// All resources (MMIO ranges, PIO ranges, SysReg ranges, IRQ lines) claimed by this device.
    fn resources(&self) -> &[Resource];

    /// Route a single guest bus access to this device.
    fn handle_access(&self, bus: BusKind, access: &BusAccess) -> BusResponse;

    // ── Optional downcasting ──────────────────────────────────────

    /// If this device is also an interrupt controller, return its ops.
    fn as_interrupt_controller(&self) -> Option<&dyn InterruptControllerOps> {
        None
    }

    /// Type-erased downcast — enables device-specific operations without
    /// modifying the core trait (crosvm uses `BusDeviceObj` for the same purpose).
    fn as_any(&self) -> &dyn Any;
}

// ============================================================
// 6. Device factory trait (registration-time)
// ============================================================

/// Creates a `VirtualDevice` from its configuration, without the VMM needing to
/// know the concrete type. This is the mechanism that eliminates the giant
/// `match` in the old `AxVmDevices::init()`.
///

pub trait DeviceFactory: Send + Sync {
    /// The device type this factory produces.
    fn emu_type(&self) -> EmulatedDeviceType;

    /// Build a device from its configuration.
    fn create(
        &self,
        config: &EmulatedDeviceConfig,
        id_alloc: &mut dyn FnMut() -> DeviceId,
    ) -> Result<Box<dyn VirtualDevice>>;
}

// Reduce re-export dependency: just enough for the factory trait
pub use axvmconfig::EmulatedDeviceConfig;
pub use axdevice_base::EmuDeviceType as EmulatedDeviceType;

#[cfg(test)]
mod tests {
    use super::*;
    use axaddrspace::device::AccessWidth as LegacyWidth;

    #[test]
    fn access_width_from_legacy_roundtrip() {
        let cases = [
            (LegacyWidth::Byte, AccessWidth::U8),
            (LegacyWidth::Word, AccessWidth::U16),
            (LegacyWidth::Dword, AccessWidth::U32),
            (LegacyWidth::Qword, AccessWidth::U64),
        ];
        for (legacy, bus) in cases {
            assert_eq!(AccessWidth::from(legacy), bus);
            assert_eq!(LegacyWidth::from(bus), legacy);
        }
    }

    #[test]
    fn bus_response_is_success() {
        assert!(BusResponse::Success(Some(42)).is_success());
        assert!(BusResponse::Success(None).is_success());
        assert!(!BusResponse::NoDevice { bus: BusKind::Mmio, addr: 0 }.is_success());
        assert!(!BusResponse::ReadOnly { bus: BusKind::Pio, addr: 0x60 }.is_success());
        assert!(!BusResponse::InvalidWidth { bus: BusKind::Mmio, addr: 0, width: AccessWidth::U8 }.is_success());
        assert!(!BusResponse::DeviceError { bus: BusKind::SysReg, addr: 0, msg: "err" }.is_success());
    }

    #[test]
    fn bus_response_value() {
        assert_eq!(BusResponse::Success(Some(0xff)).value(), Some(0xff));
        assert_eq!(BusResponse::Success(None).value(), None);
        assert_eq!(BusResponse::NoDevice { bus: BusKind::Mmio, addr: 0 }.value(), None);
    }

    #[test]
    fn bus_response_display() {
        let s = format!("{}", BusResponse::Success(Some(0xab)));
        assert!(s.contains("0xab"));

        let s = format!("{}", BusResponse::NoDevice { bus: BusKind::Mmio, addr: 0x1000 });
        assert!(s.contains("Mmio") && s.contains("0x1000"));

        let s = format!("{}", BusResponse::ReadOnly { bus: BusKind::Pio, addr: 0x60 });
        assert!(s.contains("read-only"));

        let s = format!("{}", BusResponse::DeviceError { bus: BusKind::SysReg, addr: 0x100, msg: "test" });
        assert!(s.contains("test"));
    }

    #[test]
    fn bus_access_helpers() {
        let read = BusAccess::Read { addr: 0x1000, width: AccessWidth::U32 };
        assert!(read.is_read());
        assert_eq!(read.addr(), 0x1000);
        assert_eq!(read.width(), AccessWidth::U32);

        let write = BusAccess::Write { addr: 0x2000, width: AccessWidth::U8, val: 0xff };
        assert!(!write.is_read());
        assert_eq!(write.addr(), 0x2000);
    }
}
