use tock_registers::LocalRegisterCopy;
use tock_registers::register_bitfields;
use tock_registers::registers::ReadWrite;

register_bitfields! {
    u32,
    pub SPURIOUS_INTERRUPT_VECTOR [
        /// Reserved2
        Reserved1 OFFSET(13) NUMBITS(19) [],
        /// Suppress EOI Broadcasts
        /// Determines whether an EOI for a level-triggered interrupt causes EOI messages to be broadcast to the I/O APICs (0) or not (1).
        /// See Section 11.8.5.
        /// The default value for this bit is 0, indicating that EOI broadcasts are performed.
        /// This bit is reserved to 0 if the processor does not support EOI-broadcast suppression.
        /// - 0: Disabled
        /// - 1: Enabled
        EOIBroadcastSuppression OFFSET(12) NUMBITS(1) [
            /// Disabled
            Disabled = 0,
            /// Enabled
            Enabled = 1
        ],
        Reserved0 OFFSET(10) NUMBITS(2) [],
        /// Focus Processor Checking
        /// Determines if focus processor checking is enabled (0) or disabled (1) when using the lowestpriority delivery mode.
        /// In Pentium 4 and Intel Xeon processors, this bit is reserved and should be cleared to 0.
        /// - 0: Enabled
        /// - 1: Disabled
        FocusProcessorChecking OFFSET(9) NUMBITS(1) [
            /// Enabled
            Enabled = 0,
            /// Disabled
            Disabled = 1
        ],
        /// APIC Software Enable/Disable
        /// Allows software to temporarily enable (1) or disable (0) the local APIC
        /// (see Section 11.4.3, “Enabling or Disabling the Local APIC”).
        /// - 0: APIC Disabled
        /// - 1: APIC Enabled
        APICSoftwareEnableDisable OFFSET(8) NUMBITS(1) [
            /// APIC Disabled
            Disabled = 0,
            /// APIC Enabled
            Enabled = 1
        ],
        /// Spurious Vector.
        /// Determines the vector number to be delivered to the processor when the local APIC generates a spurious vector.
        /// - (Pentium 4 and Intel Xeon processors.) Bits 0 through 7 of the this field are programmable by software.
        /// - (P6 family and Pentium processors). Bits 4 through 7 of the this field are programmable by software, and bits 0 through 3 are hardwired to logical ones. Software writes to bits 0 through 3 have no effect.
        /// For the P6 family and Pentium processors, bits 0 through 3 are always 1.
        SPURIOUS_VECTOR OFFSET(0) NUMBITS(8) [],
    ]
}

/// Spurious-Interrupt Vector Register using MMIO.
/// - Address: FEE0 00F0H
/// - Value after reset: 0000 00FFH
///
/// A special situation may occur when a processor raises its task priority to be greater than or equal to the level of the interrupt for which the processor INTR signal is currently being asserted.
/// If at the time the INTA cycle is issued, the interrupt that was to be dispensed has become masked (programmed by software), the local APIC will deliver a spurious-interrupt vector.
/// Dispensing the spurious-interrupt vector does not affect the ISR, so the handler for this vector should return without an EOI.
pub type SpuriousInterruptVectorRegisterMmio = ReadWrite<u32, SPURIOUS_INTERRUPT_VECTOR::Register>;

/// A read-write copy of Spurious-Interrupt Vector Register (FEE0 00F0H).
///
/// This behaves very similarly to a MMIO read-write register, but instead of doing a
/// volatile read to MMIO to get the value for each function call, a copy of the
/// register contents are stored locally in memory.
pub type SpuriousInterruptVectorRegisterLocal =
    LocalRegisterCopy<u32, SPURIOUS_INTERRUPT_VECTOR::Register>;
