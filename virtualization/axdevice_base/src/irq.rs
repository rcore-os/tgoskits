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

//! Architecture-neutral interrupt signaling traits for emulated devices.

use crate::DeviceError;

/// A guest-visible interrupt line used by an emulated device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct IrqLine(usize);

impl IrqLine {
    /// Creates a new interrupt line identifier.
    pub const fn new(line: usize) -> Self {
        Self(line)
    }

    /// Returns the raw interrupt line number.
    pub const fn number(self) -> usize {
        self.0
    }
}

/// The target of an interrupt operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IrqTarget {
    /// Route to the architecture-defined default target.
    Default,
    /// Broadcast to all active virtual CPUs.
    Broadcast,
    /// Route to one virtual CPU.
    Vcpu(usize),
    /// Route to a mask of virtual CPUs.
    VcpuMask(u64),
}

/// A message-signaled interrupt payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MsiMessage {
    /// Guest-programmed MSI address.
    pub address: u64,
    /// Guest-programmed MSI data.
    pub data: u32,
    /// Optional logical target interpreted by the interrupt router.
    pub target: Option<IrqTarget>,
}

impl MsiMessage {
    /// Creates a new MSI message with no explicit target override.
    pub const fn new(address: u64, data: u32) -> Self {
        Self {
            address,
            data,
            target: None,
        }
    }

    /// Creates a new MSI message with an explicit target.
    pub const fn with_target(address: u64, data: u32, target: IrqTarget) -> Self {
        Self {
            address,
            data,
            target: Some(target),
        }
    }
}

/// Device-facing interrupt sink.
///
/// Implementations translate these semantic operations into the architecture-specific
/// interrupt controller backend, such as vIOAPIC/vLAPIC, VGIC, vPLIC/AIA, or LoongArch
/// virtual interrupt state.
pub trait IrqSink {
    /// Assert a level interrupt line.
    fn raise(&self, line: IrqLine) -> Result<(), DeviceError>;

    /// Deassert a level interrupt line.
    fn lower(&self, line: IrqLine) -> Result<(), DeviceError>;

    /// Generate an edge-style interrupt pulse.
    fn pulse(&self, line: IrqLine) -> Result<(), DeviceError>;

    /// Deliver a message-signaled interrupt.
    fn msi(&self, message: MsiMessage) -> Result<(), DeviceError>;

    /// Notify end-of-interrupt for a line when the backend needs it.
    fn eoi(&self, line: IrqLine) -> Result<(), DeviceError>;
}
