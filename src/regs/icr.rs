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

//! 11.6.1 Interrupt Command Register (ICR)
//! The interrupt command register (ICR) is a 64-bit1 local APIC register (see Figure 11-12)
//! that allows software running on the processor to specify and send interprocessor interrupts (IPIs) to other processors in the system.

use tock_registers::LocalRegisterCopy;
use tock_registers::register_bitfields;
use tock_registers::registers::ReadWrite;

register_bitfields! {
    u32,
    pub INTERRUPT_COMMAND_LOW [
        /// Reserved
        Reserved2 OFFSET(20) NUMBITS(12) [],
        /// Destination Shorthand
        /// Indicates whether a shorthand notation is used to specify the destination of the interrupt and, if so, which shorthand is used. Destination shorthands are used in place of the 8-bit destination field, and can be sent by software using a single write to the low doubleword of the ICR. Shorthands are defined for the following cases: software self interrupt, IPIs to all processors in the system including the sender, IPIs to all processors in the system excluding the sender.
        /// - 00: (No Shorthand)
        ///     The destination is specified in the destination field.
        /// - 01: (Self)
        ///     The issuing APIC is the one and only destination of the IPI. This destination shorthand allows software to interrupt the processor on which it is executing. An APIC implementation is free to deliver the self-interrupt message internally or to issue the message to the bus and “snoop” it as with any other IPI message.
        /// - 10: (All Including Self)
        ///     The IPI is sent to all processors in the system including the processor sending the IPI. The APIC will broadcast an IPI message with the destination field set to FH for Pentium and P6 family processors and to FFH for Pentium 4 and Intel Xeon processors.
        /// - 11: (All Excluding Self)
        ///     The IPI is sent to all processors in a system with the exception of the processor sending the IPI. The APIC broadcasts a message with the physical destination mode and destination field set to FH for Pentium and P6 family processors and to FFH for Pentium 4 and Intel Xeon processors. Support for this destination shorthand in conjunction with the lowest-priority delivery mode is model specific. For Pentium 4 and Intel Xeon processors, when this shorthand is used together with lowest priority delivery mode, the IPI may be redirected back to the issuing processor.
        DestinationShorthand OFFSET(18) NUMBITS(2) [
            /// No Shorthand
            NoShorthand = 0b00,
            /// Self
            SELF = 0b01,
            /// All Including Self
            AllIncludingSelf = 0b10,
            /// All Excluding Self
            AllExcludingSelf = 0b11
        ],
        /// Reserved
        Reserved1 OFFSET(16) NUMBITS(2) [],
        /// Trigger Mode
        /// Selects the trigger mode when using the INIT level de-assert delivery mode:
        ///     edge (0) or level (1).
        /// It is ignored for all other delivery modes.
        /// (This flag has no meaning in Pentium 4 and Intel Xeon processors, and will always be issued as a 0.)
        TriggerMode OFFSET(15) NUMBITS(1) [
            /// Edge
            Edge = 0,
            /// Level
            Level = 1
        ],
        /// Level
        /// For the INIT level de-assert delivery mode this flag must be set to 0;
        /// for all other delivery modes it must be set to 1.
        ///  (This flag has no meaning in Pentium 4 and Intel Xeon processors, and will always be issued as a 1.)
        Level OFFSET(14) NUMBITS(1) [
            /// De-assert
            DeAssert = 0,
            /// Assert
            Assert = 1
        ],
        /// Reserved
        Reserved0 OFFSET(13) NUMBITS(1) [],
        /// Delivery Status (Read Only) Indicates the IPI delivery status, as follows:
        /// - 0 (Idle) Indicates that this local APIC has completed sending any previous IPIs.
        /// - 1 (Send Pending) Indicates that this local APIC has not completed sending the last IPI.
        DeliveryStatus OFFSET(12) NUMBITS(1) [
            /// Idle
            Idle = 0,
            /// Send Pending
            SendPending = 1
        ],
        /// Destination Mode Selects either physical (0) or logical (1) destination mode (see Section 11.6.2, “Determining IPI Destination”).
        DestinationMode OFFSET(11) NUMBITS(1) [
            /// Physical
            Physical = 0,
            /// Logical
            Logical = 1
        ],
        /// Delivery Mode Specifies the type of IPI to be sent.
        /// This field is also know as the IPI message type field.
        /// - 000 (Fixed)
        ///     Delivers the interrupt specified in the vector field to the target processor or processors.
        /// - 001 (Lowest Priority)
        ///     Same as fixed mode, except that the interrupt is delivered to the processor executing at the lowest priority among the set of processors specified in the destination field. The ability for a processor to send a lowest priority IPI is model specific and should be avoided by BIOS and operating system software.
        /// - 010 (SMI)
        ///     Delivers an SMI interrupt to the target processor or processors. The vector field must be programmed to 00H for future compatibility.
        /// - 011 (Reserved)
        /// - 100 (NMI)
        ///     Delivers an NMI interrupt to the target processor or processors. The vector information is ignored.
        /// - 101 (INIT)
        ///     Delivers an INIT request to the target processor or processors, which causes them to perform an INIT.
        ///     As a result of this IPI message, all the target processors perform an INIT.
        ///     The vector field must be programmed to 00H for future compatibility.
        /// - 101 (INIT Level De-assert)
        ///     (Not supported in the Pentium 4 and Intel Xeon processors.)
        ///     Sends a synchronization message to all the local APICs in the system to set their arbitration IDs (stored in their Arb ID registers) to the values of their APIC IDs (see Section 11.7, “System and APIC Bus Arbitration”).
        ///     For this delivery mode, the level flag must be set to 0 and trigger mode flag to 1.
        ///     This IPI is sent to all processors, regardless of the value in the destination field or the destination shorthand field;
        ///     however, software should specify the “all including self” shorthand.
        /// - 110 (Start-Up)
        ///     Sends a special “start-up” IPI (called a SIPI) to the target processor or processors.
        ///     The vector typically points to a start-up routine that is part of the BIOS boot-strap code
        ///     (see Section 9.4, “Multiple-Processor (MP) Initialization”).
        ///     IPIs sent with this delivery mode are not automatically retried if the source APIC is unable to deliver it.
        ///     It is up to the software to determine if the SIPI was not successfully delivered and to reissue the SIPI if necessary.
        /// - 111 (Reserved)
        DeliveryMode OFFSET(8) NUMBITS(3) [
            /// Fixed
            Fixed = 0b000,
            /// Lowest Priority
            LowestPriority = 0b001,
            /// SMI
            SMI = 0b010,
            /// Reserved
            Reserved011 = 0b011,
            /// NMI
            NMI = 0b100,
            /// INIT
            INIT = 0b101,
            /// Start-Up
            StartUp = 0b110,
            /// Start-Up
            Reserved111 = 0b111
        ],
        /// Vector The vector number of the interrupt being sent.
        Vector OFFSET(0) NUMBITS(8) []
    ]
}

register_bitfields! {
    u32,
    pub INTERRUPT_COMMAND_HIGH [
        /// Destination [56:63]
        /// Specifies the target processor or processors.
        /// This field is only used when the destination shorthand field is set to 00B.
        /// If the destination mode is set to physical, then bits 56 through 59 contain the APIC ID of the target processor for Pentium and P6 family processors and bits 56 through 63 contain the APIC ID of the target processor the for Pentium 4 and Intel Xeon processors.
        /// If the destination mode is set to logical, the interpretation of the 8-bit destination field depends on the settings of the DFR and LDR registers of the local APICs in all the processors in the system
        /// (see Section 11.6.2, “Determining IPI Destination”).
        ///
        /// 11.12.9 ICR Operation in x2APIC Mode
        /// The destination ID field is expanded to 32 bits in x2APIC mode.
        Destination OFFSET(24) NUMBITS(8) [],
        /// Reserved
        Reserved OFFSET(0) NUMBITS(24) []
    ]
}

/// Interrupt Command Register (ICR) LOW using MMIO.
/// - Address: FEE0 0300H (0 - 31)
/// - Value after Reset: 0H
pub type InterruptCommandRegisterLowMmio = ReadWrite<u32, INTERRUPT_COMMAND_LOW::Register>;

/// A read-write copy of Interrupt Command Register (ICR) LOW.
/// This behaves very similarly to a MMIO read-write register, but instead of doing a
/// volatile read to MMIO to get the value for each function call, a copy of the
/// register contents are stored locally in memory.
pub type InterruptCommandRegisterLowLocal = LocalRegisterCopy<u32, INTERRUPT_COMMAND_LOW::Register>;

/// Interrupt Command Register (ICR) HIGH using MMIO.
/// - Address: FEE0 0310H (32 - 63)
/// - Value after Reset: 0H
pub type InterruptCommandRegisterHighMmio = ReadWrite<u32, INTERRUPT_COMMAND_HIGH::Register>;
