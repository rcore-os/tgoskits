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

//! Figure 11-9. Error Status Register (ESR)
//! 11.5.3 Error Handling
//! The local APIC records errors detected during interrupt handling in the error status register (ESR).

use tock_registers::LocalRegisterCopy;
use tock_registers::fields::FieldValue;
use tock_registers::register_bitfields;
use tock_registers::registers::ReadWrite;

register_bitfields! {
    u32,
    pub ERROR_STATUS [
        /// Reserved
        Reserved2 OFFSET(8) NUMBITS(24) [],
        /// Bit 7: Illegal Register Address
        /// Set when the local APIC is in xAPIC mode and software attempts to access a register that is reserved in the processor's local-APIC register-address space; see Table 10-1.
        /// (The local-APIC register-address space comprises the 4 KBytes at the physical address specified in the IA32_APIC_BASE MSR.)
        /// Used only on Intel Core, Intel Atom, Pentium 4, Intel Xeon, and P6 family processors.
        IllegalRegisterAddress OFFSET(7) NUMBITS(1) [],
        /// Bit 6: Receive Illegal Vector.
        /// Set when the local APIC detects an illegal vector (one in the range 0 to 15) in an interrupt message it receives or in an interrupt generated locally from the local vector table or via a self IPI.
        /// Such interrupts are not delivered to the processor; the local APIC will never set an IRR bit in the range 0 to 15.
        ReceiveIllegalVector OFFSET(6) NUMBITS(1) [],
        /// Bit 5: Send Illegal Vector.
        /// Set when the local APIC detects an illegal vector (one in the range 0 to 15) in the message that it is sending.
        /// This occurs as the result of a write to the ICR (in both xAPIC and x2APIC modes) or to SELF IPI register (x2APIC mode only) with an illegal vector.
        /// If the local APIC does not support the sending of lowest-priority IPIs and software writes the ICR to send a lowest-priority IPI with an illegal vector, the local APIC sets only the “redirectable IPI” error bit.
        ///  The interrupt is not processed and hence the “Send Illegal Vector” bit is not set in the ESR.
        SendIllegalVector OFFSET(5) NUMBITS(1) [],
        /// Bit 4: Redirectable IPI.
        /// Set when the local APIC detects an attempt to send an IPI with the lowest-priority delivery mode and the local APIC does not support the sending of such IPIs.
        /// This bit is used on some Intel Core and Intel Xeon processors. ]
        /// As noted in Section 11.6.2, the ability of a processor to send a lowest-priority IPI is model-specific and should be avoided.
        RedirectableIPI OFFSET(4) NUMBITS(1) [],
        /// Bit 3: Receive Accept Error.
        /// Set when the local APIC detects that the message it received was not accepted by any APIC on the APIC bus, including itself.
        /// Used only on P6 family and Pentium processors.
        ReceiveAcceptError OFFSET(3) NUMBITS(1) [],
        /// Bit 2: Send Accept Error.
        /// Set when the local APIC detects that a message it sent was not accepted by any APIC on the APIC bus.
        /// Used only on P6 family and Pentium processors.
        SendAcceptError OFFSET(2) NUMBITS(1) [],
        /// Bit 1: Receive Checksum Error.
        /// Set when the local APIC detects a checksum error for a message that it received on the APIC bus.
        /// Used only on P6 family and Pentium processors.
        ReceiveChecksumError OFFSET(1) NUMBITS(1) [],
        /// Bit 0: Send Checksum Error.
        /// Set when the local APIC detects a checksum error for a message that it sent on the APIC bus.
        /// Used only on P6 family and Pentium processors.
        SendChecksumError OFFSET(0) NUMBITS(1) [],
    ]
}

/// Error Status Register (ESR) using MMIO.
/// The local APIC records errors detected during interrupt handling in the error status register (ESR).
/// - Address: FEE0 0280H
/// - Value after reset: 0H
pub type ErrorStatusRegisterMmio = ReadWrite<u32, ERROR_STATUS::Register>;

/// A read-write copy of Error Status Register (ESR).
/// This behaves very similarly to a MMIO read-write register, but instead of doing a
/// volatile read to MMIO to get the value for each function call, a copy of the
/// register contents are stored locally in memory.
pub type ErrorStatusRegisterLocal = LocalRegisterCopy<u32, ERROR_STATUS::Register>;

pub type ErrorStatusRegisterValue = FieldValue<u32, ERROR_STATUS::Register>;
