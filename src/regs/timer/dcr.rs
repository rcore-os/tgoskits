use tock_registers::LocalRegisterCopy;
use tock_registers::register_bitfields;
use tock_registers::registers::ReadWrite;

register_bitfields! {
    u32,
    pub DCR_TIMER [
        /// Reserved
        Reserved OFFSET(4) NUMBITS(28) [],
        /// Divide Value (bits 0, 1, and 3)
        /// 000: Divide by 2
        /// 001: Divide by 4
        /// 010: Divide by 8
        /// 011: Divide by 16
        /// 100: Divide by 32
        /// 101: Divide by 64
        /// 110: Divide by 128
        /// 111: Divide by 1
        DivideValue OFFSET(0) NUMBITS(4) [
            DivideBy2 = 0b0000,
            DivideBy4 = 0b0001,
            DivideBy8 = 0b0010,
            DivideBy16 = 0b0011,
            DivideBy32 = 0b1000,
            DivideBy64 = 0b1001,
            DivideBy128 = 0b1010,
            DivideBy1 = 0b1011
        ]
    ]
}

/// Divide Configuration Register (FEE0 03E0H) using MMIO.
/// - Address: FEE0 03E0H
/// - Value after reset: 0H
pub type DivideConfigurationRegisterMmio = ReadWrite<u32, DCR_TIMER::Register>;

/// A read-write copy of the Divide Configuration Register (FEE0 03E0H).
///
/// This behaves very similarly to a MMIO read-write register, but instead of doing a
/// volatile read to MMIO to get the value for each function call, a copy of the
/// register contents are stored locally in memory.
#[allow(dead_code)]
pub type DivideConfigurationRegisterLocal = LocalRegisterCopy<u32, DCR_TIMER::Register>;
