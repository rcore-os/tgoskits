use tock_registers::LocalRegisterCopy;
use tock_registers::register_bitfields;
use tock_registers::registers::ReadWrite;

register_bitfields! {
    u32,
    pub LVT_TIMER [
        /// Reserved2
        Reserved2 OFFSET(19) NUMBITS(13) [],
        /// Timer Mode
        TimerMode OFFSET(17) NUMBITS(2) [
            /// (00b) one-shot mode using a count-down value
            OneShot = 0b00,
            /// (01b) periodic mode reloading a count-down value
            Periodic = 0b01,
            /// (10b) TSC-Deadline mode using absolute target value in IA32_TSC_DEADLINE MSR (see Section 11.5.4.1)
            TSCDeadline = 0b10,
            /// (11b) is reserved
            Reserved = 0b11
        ],
        /// Mask: Interrupt mask:
        /// (0) enables reception of the interrupt and (1) inhibits reception of the interrupt.
        /// When the local APIC handles a performance-monitoring counters interrupt,
        /// it automatically sets the mask flag in the LVT performance counter register.
        /// This flag is set to 1 on reset.
        /// It can be cleared only by software.
        Mask OFFSET(16) NUMBITS(1) [
            /// Not masked, enables reception of the interrupt.
            NotMasked = 0,
            /// Masked, inhibits reception of the interrupt.
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
        Reserved0 OFFSET(8) NUMBITS(4) [],
        /// Vector: Interrupt vector number.
        Vector OFFSET(0) NUMBITS(8) [],
    ]
}

/// LVT Timer Register (FEE0 0320H)
pub type LvtTimerRegisterMmio = ReadWrite<u32, LVT_TIMER::Register>;

/// A read-write copy of LVT Timer Register (FEE0 0320H).
///
/// This behaves very similarly to a MMIO read-write register, but instead of doing a
/// volatile read to MMIO to get the value for each function call, a copy of the
/// register contents are stored locally in memory.
pub type LvtTimerRegisterLocal = LocalRegisterCopy<u32, LVT_TIMER::Register>;
