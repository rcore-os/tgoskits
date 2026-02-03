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

//! 11.4.4 Local APIC Status and Location
//! The status and location of the local APIC are contained in the IA32_APIC_BASE MSR (see Figure 11-5).
//! Figure 11-26. IA32_APIC_BASE MSR Supporting x2APIC
//! Processor support for x2APIC mode can be detected by executing CPUID with EAX=1 and then checking ECX, bit 21 ECX.
//! If CPUID.(EAX=1):ECX.21 is set , the processor supports the x2APIC capability and can be placed into the x2APIC mode.
//! System software can place the local APIC in the x2APIC mode by setting the x2APIC mode enable bit (bit 10) in the IA32_APIC_BASE MSR at MSR address 01BH.

use tock_registers::LocalRegisterCopy;
use tock_registers::register_bitfields;

register_bitfields! {
    u64,
    pub APIC_BASE [
        /// Reserved2
        Reserved2 OFFSET(36) NUMBITS(28) [],
        /// APIC Base field, bits 12 through 35
        /// Specifies the base address of the APIC registers.
        /// This 24-bit value is extended by 12 bits at the low end to form the base address.
        /// This automatically aligns the address on a 4-KByte boundary.
        /// Following a power-up or reset, the field is set to FEE0 0000H.
        APIC_BASE OFFSET(12) NUMBITS(24) [],
        /// APIC Global Enable flag, bit 11
        /// Enables or disables the local APIC (see Section 11.4.3, “Enabling or Disabling the Local APIC”).
        /// This flag is available in the Pentium 4, Intel Xeon, and P6 family processors.
        /// It is not guaranteed to be available or available at the same location in future Intel 64 or IA-32 processors.
        /// EN—xAPIC global enable/disable
        /// - 0: xAPIC disabled
        /// - 1: xAPIC enabled
        XAPIC_ENABLED OFFSET(11) NUMBITS(1) [],
        /// EXTD—Enable x2APIC mode
        /// - 0: xAPIC mode
        /// - 1: x2APIC mode
        X2APIC_Enabled OFFSET(10) NUMBITS(1) [],
        /// Reserved1
        Reserved1 OFFSET(9) NUMBITS(1) [],
        /// BSP flag, bit 8
        /// Indicates if the processor is the bootstrap processor (BSP).
        /// See Section 9.4, “MultipleProcessor (MP) Initialization.”
        /// Following a power-up or reset, this flag is set to 1 for the processor selected as the BSP and set to 0 for the remaining processors (APs).
        /// - 0: Processor is not BSP
        /// - 1: Processor is BSP
        BSP OFFSET(8) NUMBITS(1) [],
        /// Reserved0
        Reserved0 OFFSET(0) NUMBITS(8) [],
    ]
}

/// IA32_APIC_BASE MSR (Model Specific Register) supporting x2APIC.
/// - Address: 1B0H
/// - Value after reset: FEE_0000_0000H
///
/// Table 11-5, "x2APIC operating mode configurations" describe the possible combinations of the enable bit (EN - bit 11)
/// and the extended mode bit (EXTD - bit 10) in the IA32_APIC_BASE MSR.
///
/// (xAPIC global enable (IA32_APIC_BASE[11]),x2APIC enable (IA32_APIC_BASE[10])) = Description
///
/// - (0, 0) = local APIC is disabled
/// - (0, 1) = Invalid
/// - (1, 0) = local APIC is enabled in xAPIC mode
/// - (1, 1) = local APIC is enabled in x2APIC mode
pub type ApicBaseRegisterMsr = LocalRegisterCopy<u64, APIC_BASE::Register>;
