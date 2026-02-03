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

use tock_registers::LocalRegisterCopy;
use tock_registers::register_bitfields;
use tock_registers::registers::ReadWrite;

register_bitfields! {
    u32,
    pub DESTINATION_FORMAT [
        /// Model
        Model OFFSET(28) NUMBITS(4) [
            /// Flat model
            Flat = 0b1111,
            /// Cluster model
            Cluster = 0b0000
        ],
        /// Reserved (All 1s)
        ReservedALL1 OFFSET(0) NUMBITS(28) [],
    ]
}

/// Destination Format Register using MMIO.
/// - Address: FEE0 00E0H
/// - Value after reset: FFFF FFFFH
///
/// The 4-bit model field in this register selects one of two models (flat or cluster) that can be used to interpret the MDA when using logical destination mode.
/// **Flat Model** — This model is selected by programming DFR bits 28 through 31 to 1111.
/// Here, a unique logical APIC ID can be established for up to 8 local APICs by setting a different bit in the logical APIC ID field of the LDR for each local APIC.
/// A group of local APICs can then be selected by setting one or more bits in the MDA.
///
/// **Cluster Model** — This model is selected by programming DFR bits 28 through 31 to 0000.
/// This model supports two basic destination schemes: flat cluster and hierarchical cluster.
pub type DestinationFormatRegisterMmio = ReadWrite<u32, DESTINATION_FORMAT::Register>;

/// A read-write copy of Destination Format Register (FEE0 00E0H).
///
/// This behaves very similarly to a MMIO read-write register, but instead of doing a
/// volatile read to MMIO to get the value for each function call, a copy of the
/// register contents are stored locally in memory.
#[allow(dead_code)]
pub type DestinationFormatRegisterLocal = LocalRegisterCopy<u32, DESTINATION_FORMAT::Register>;
