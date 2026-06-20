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

//! Resource and capability declarations for emulated devices.

use axvm_types::GuestPhysAddrRange;

use crate::{IrqLine, PortRange, SysRegAddrRange};

/// The kind of PCI BAR exposed by a device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PciBarKind {
    /// A 32-bit memory BAR.
    Mmio32 {
        /// Whether the BAR is prefetchable.
        prefetchable: bool,
    },
    /// A 64-bit memory BAR.
    Mmio64 {
        /// Whether the BAR is prefetchable.
        prefetchable: bool,
    },
    /// A port I/O BAR.
    Pio,
}

/// A resource occupied or requested by an emulated device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resource {
    /// A guest physical MMIO range.
    Mmio(GuestPhysAddrRange),
    /// A port I/O range.
    Pio(PortRange),
    /// A system register range.
    SysReg(SysRegAddrRange),
    /// A legacy interrupt line.
    Irq(IrqLine),
    /// A message-signaled interrupt vector allocation request.
    Msi {
        /// Number of MSI/MSI-X vectors requested or supported.
        vectors: u16,
    },
    /// The device can initiate DMA to guest memory.
    Dma,
    /// A PCI BAR exposed by the device.
    PciBar {
        /// BAR index in PCI configuration space.
        index: u8,
        /// BAR type and attributes.
        kind: PciBarKind,
    },
}

/// Capability flags exposed by an emulated device.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DeviceCapabilities {
    /// The device supports MSI.
    pub msi: bool,
    /// The device supports MSI-X.
    pub msix: bool,
    /// The device may initiate DMA.
    pub dma: bool,
    /// The device exposes one or more PCI BARs.
    pub pci_bar: bool,
    /// The device has a meaningful reset operation.
    pub reset: bool,
    /// The device has a meaningful suspend operation.
    pub suspend: bool,
    /// The device has a meaningful resume operation.
    pub resume: bool,
}

impl DeviceCapabilities {
    /// No optional capabilities.
    pub const NONE: Self = Self {
        msi: false,
        msix: false,
        dma: false,
        pci_bar: false,
        reset: false,
        suspend: false,
        resume: false,
    };

    /// Returns an empty capability set.
    pub const fn none() -> Self {
        Self::NONE
    }
}
