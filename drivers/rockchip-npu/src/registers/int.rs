use tock_registers::register_structs;
use tock_registers::registers::*;

register_structs! {
    /// Int register block starting at offset 0 relative to the int group base.
    pub IntRegs {
        (0x00 => pub int_mask: ReadWrite<u32>),
        (0x04 => pub int_clear: WriteOnly<u32>),
        (0x08 => pub int_status: ReadOnly<u32>),
        (0x0C => pub int_raw_status: ReadOnly<u32>),
        (0x10 => @END),
    }
}

tock_registers::register_bitfields! {u32,
    INT_REGS [
        IRQ0 OFFSET(0) NUMBITS(1) [],
        IRQ1 OFFSET(1) NUMBITS(1) [],
        IRQ2 OFFSET(2) NUMBITS(1) [],
        IRQ_ALL OFFSET(0) NUMBITS(32) []
    ]
}
