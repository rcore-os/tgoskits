use tock_registers::register_structs;
use tock_registers::registers::ReadWrite;

register_structs! {
    pub GlobalRegs {
        (0x0000 => _reserved0),
        (0x0008 => pub enable_mask: ReadWrite<u32, GLOBAL::Register>),
        (0x000C => @END),
    }
}

tock_registers::register_bitfields! {u32,
    GLOBAL [
        ENABLE_MASK OFFSET(0) NUMBITS(32) []
    ]
}
