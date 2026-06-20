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

//! Unified bus transaction types for emulated device access.

use axvm_types::GuestPhysAddr;

use crate::{AccessWidth, Port, SysRegAddr};

/// The kind of guest-visible bus or register namespace used for a device access.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BusKind {
    /// Memory-mapped I/O in the guest physical address space.
    Mmio,
    /// Port I/O, primarily used by x86 `in`/`out` instructions.
    Pio,
    /// Architecture system register namespace such as MSR, CSR, or AArch64 sysreg.
    SysReg,
}

/// A bus address tagged with the namespace it belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum BusAddress {
    /// A guest physical MMIO address.
    Mmio(GuestPhysAddr),
    /// A port I/O address.
    Pio(Port),
    /// A system register address.
    SysReg(SysRegAddr),
}

impl BusAddress {
    /// Returns the bus kind implied by this address.
    pub const fn kind(self) -> BusKind {
        match self {
            Self::Mmio(_) => BusKind::Mmio,
            Self::Pio(_) => BusKind::Pio,
            Self::SysReg(_) => BusKind::SysReg,
        }
    }
}

/// The operation requested by a bus access.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BusOp {
    /// Read from the addressed device register.
    Read,
    /// Write a value to the addressed device register.
    Write {
        /// The value to write. Only the low bits selected by [`BusAccess::width`] are significant.
        value: usize,
    },
}

/// A normalized device access generated from a VM exit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BusAccess {
    /// The bus namespace used by the access.
    pub kind: BusKind,
    /// The address in the selected bus namespace.
    pub addr: BusAddress,
    /// The access width.
    pub width: AccessWidth,
    /// The requested operation.
    pub op: BusOp,
}

impl BusAccess {
    /// Creates a new bus access.
    pub const fn new(kind: BusKind, addr: BusAddress, width: AccessWidth, op: BusOp) -> Self {
        Self {
            kind,
            addr,
            width,
            op,
        }
    }

    /// Creates an MMIO read access.
    pub const fn mmio_read(addr: GuestPhysAddr, width: AccessWidth) -> Self {
        Self::new(BusKind::Mmio, BusAddress::Mmio(addr), width, BusOp::Read)
    }

    /// Creates an MMIO write access.
    pub const fn mmio_write(addr: GuestPhysAddr, width: AccessWidth, value: usize) -> Self {
        Self::new(
            BusKind::Mmio,
            BusAddress::Mmio(addr),
            width,
            BusOp::Write { value },
        )
    }

    /// Creates a port I/O read access.
    pub const fn pio_read(port: Port, width: AccessWidth) -> Self {
        Self::new(BusKind::Pio, BusAddress::Pio(port), width, BusOp::Read)
    }

    /// Creates a port I/O write access.
    pub const fn pio_write(port: Port, width: AccessWidth, value: usize) -> Self {
        Self::new(
            BusKind::Pio,
            BusAddress::Pio(port),
            width,
            BusOp::Write { value },
        )
    }

    /// Creates a system register read access.
    pub const fn sysreg_read(addr: SysRegAddr, width: AccessWidth) -> Self {
        Self::new(
            BusKind::SysReg,
            BusAddress::SysReg(addr),
            width,
            BusOp::Read,
        )
    }

    /// Creates a system register write access.
    pub const fn sysreg_write(addr: SysRegAddr, width: AccessWidth, value: usize) -> Self {
        Self::new(
            BusKind::SysReg,
            BusAddress::SysReg(addr),
            width,
            BusOp::Write { value },
        )
    }
}

/// The result returned by a device for a bus access.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BusResponse {
    /// A read completed and returned a value.
    Read {
        /// The read value, zero-extended by the device implementation when needed.
        value: usize,
    },
    /// A write completed.
    Write,
}
