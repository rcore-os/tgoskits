use tock_registers::LocalRegisterCopy;
use tock_registers::register_bitfields;
use tock_registers::registers::ReadWrite;

register_bitfields! {
    u32,
    pub LVT_CMCI [
        /// Reserved2
        Reserved2 OFFSET(17) NUMBITS(15) [],
        /// Mask
        Mask OFFSET(16) NUMBITS(1) [
            /// Not masked.
            NotMasked = 0,
            /// Masked.
            Masked = 1
        ],
        Reserved1 OFFSET(13) NUMBITS(3) [],
        /// Delivery Status (Read Only): Indicates the interrupt delivery status
        DeliveryStatus OFFSET(12) NUMBITS(1) [
            /// 0 (Idle)
            /// There is currently no activity for this interrupt source,
            /// or the previous interrupt from this source was delivered to the processor core and accepted.
            Idle = 0,
            /// 1 (Send Pending)
            /// Indicates that an interrupt from this source has been delivered to the processor core
            /// but has not yet been accepted (see Section 11.5.5, “Local Interrupt Acceptance”).
            SendPending = 1
        ],
        Reserved0 OFFSET(11) NUMBITS(1) [],
        /// Delivery Mode: Specifies the type of interrupt to be sent to the processor.
        /// Some delivery modes will only operate as intended when used in conjunction with a specific trigger mode.
        DeliveryMode OFFSET(8) NUMBITS(3) [
            /// 000 (Fixed) Delivers the interrupt specified in the vector field.
            Fixed = 0b000,
            /// 010 (SMI) Delivers an SMI interrupt to the processor core through
            /// the processor’s local SMI signal path.
            /// When using this delivery mode, the vector field should be set to 00H for future compatibility.
            SMI = 0b010,
            /// 100 (NMI) Delivers an NMI interrupt to the processor. The vector information is ignored.
            NMI = 0b100,
            /// 101 (INIT) Delivers an INIT request to the processor core,
            /// which causes the processor to perform an INIT.
            /// When using this delivery mode, the vector field should be set to 00H for future compatibility.
            /// Not supported for the LVT CMCI register, the LVT thermal monitor register, or the LVT performance counter register.
            INIT = 0b101,
            /// 110 Reserved; not supported for any LVT register.
            Reserved = 0b110,
            /// 111 (ExtINT) Causes the processor to respond to the interrupt
            /// as if the interrupt originated in an externally connected (8259A-compatible) interrupt controller.
            /// A special INTA bus cycle corresponding to ExtINT, is routed to the external controller.
            /// The external controller is expected to supply the vector information.
            /// The APIC architecture supports only one ExtINT source in a system, usually contained in the compatibility bridge.
            /// Only one processor in the system should have an LVT entry configured to use the ExtINT delivery mode.
            /// Not supported for the LVT CMCI register, the LVT thermal monitor register, or the LVT performance counter register.
            ExtINT = 0b111
        ],
        /// Vector: Interrupt vector number.
        Vector OFFSET(0) NUMBITS(8) [],
    ]
}

/// LVT CMCI Register (FEE0 02F0H) using MMIO.
/// Specifies interrupt delivery when an overflow condition of corrected machine check error count
/// reaching a threshold value occurred in a machine check bank supporting CMCI
/// (see Section 16.5.1, “CMCI Local APIC Interface”).
pub type LvtCmciRegisterMmio = ReadWrite<u32, LVT_CMCI::Register>;

/// A read-write copy of LVT CMCI Register (FEE0 02F0H).
///
/// This behaves very similarly to a MMIO read-write register, but instead of doing a
/// volatile read to MMIO to get the value for each function call, a copy of the
/// register contents are stored locally in memory.
pub type LvtCmciRegisterLocal = LocalRegisterCopy<u32, LVT_CMCI::Register>;
