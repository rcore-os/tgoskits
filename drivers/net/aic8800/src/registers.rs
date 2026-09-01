//! AIC SDIO function-register layout.
//!
//! These are card function-space registers reached through CMD52/CMD53, not
//! processor MMIO. `tock-registers` still provides one checked definition of
//! every bitfield used by the protocol state machines.

use tock_registers::register_bitfields;

register_bitfields! [
    u8,

    pub FLOW_CONTROL [
        CREDITS OFFSET(0) NUMBITS(7) []
    ],

    pub INTERRUPT_STATUS [
        BLOCK_COUNT OFFSET(0) NUMBITS(7) [],
        OTHER OFFSET(7) NUMBITS(1) []
    ],

    pub SLEEP_STATUS [
        READY OFFSET(4) NUMBITS(1) []
    ],

    pub INTERRUPT_ENABLE [
        DATA OFFSET(0) NUMBITS(1) [],
        COMMAND OFFSET(1) NUMBITS(1) [],
        ERROR OFFSET(2) NUMBITS(1) []
    ]
];

pub(crate) const INTERRUPTS_ENABLED: u8 = INTERRUPT_ENABLE::DATA::SET.value
    | INTERRUPT_ENABLE::COMMAND::SET.value
    | INTERRUPT_ENABLE::ERROR::SET.value;

#[derive(Clone, Copy)]
enum ReceiveLengthEncoding {
    V1,
    V3,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ReceiveLength {
    Empty,
    Blocks(u8),
    ByteMode,
    OtherInterrupt,
}

#[derive(Clone, Copy)]
pub(crate) struct RegisterMap {
    pub block_count: u32,
    pub byte_mode_length: u32,
    pub byte_mode_enable: u32,
    pub flow_control: u32,
    pub interrupt_enable: u32,
    pub read_fifo: u32,
    pub sleep_status: Option<u32>,
    pub wakeup: Option<u32>,
    pub write_fifo: u32,
    receive_length_encoding: ReceiveLengthEncoding,
}

impl RegisterMap {
    pub(crate) const fn v3() -> Self {
        Self {
            block_count: 0x04,
            byte_mode_length: 0x05,
            byte_mode_enable: 0x07,
            flow_control: 0x03,
            interrupt_enable: 0x00,
            read_fifo: 0x0f,
            sleep_status: Some(0x01),
            wakeup: Some(0x02),
            write_fifo: 0x10,
            receive_length_encoding: ReceiveLengthEncoding::V3,
        }
    }

    pub(crate) const fn v1() -> Self {
        Self {
            block_count: 0x12,
            byte_mode_length: 0x02,
            byte_mode_enable: 0x11,
            flow_control: 0x0a,
            interrupt_enable: 0x04,
            read_fifo: 0x08,
            sleep_status: None,
            wakeup: Some(0x09),
            write_fifo: 0x07,
            receive_length_encoding: ReceiveLengthEncoding::V1,
        }
    }

    pub(crate) fn receive_length(self, value: u8) -> ReceiveLength {
        match self.receive_length_encoding {
            ReceiveLengthEncoding::V1 => match value {
                0 => ReceiveLength::Empty,
                1..64 => ReceiveLength::Blocks(value),
                _ => ReceiveLength::ByteMode,
            },
            ReceiveLengthEncoding::V3 => {
                if value & INTERRUPT_STATUS::OTHER::SET.value != 0 {
                    ReceiveLength::OtherInterrupt
                } else if value == 0 {
                    ReceiveLength::Empty
                } else {
                    ReceiveLength::Blocks(
                        (value & INTERRUPT_STATUS::BLOCK_COUNT.mask)
                            >> INTERRUPT_STATUS::BLOCK_COUNT.shift,
                    )
                }
            }
        }
    }
}

pub(crate) fn flow_credits(value: u8) -> u8 {
    (value & FLOW_CONTROL::CREDITS.mask) >> FLOW_CONTROL::CREDITS.shift
}

pub(crate) fn interface_ready(value: u8) -> bool {
    value & SLEEP_STATUS::READY::SET.value != 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v3_interrupt_status_keeps_other_source_separate_from_block_count() {
        assert_eq!(
            RegisterMap::v3().receive_length(3),
            ReceiveLength::Blocks(3)
        );
        assert_eq!(
            RegisterMap::v3().receive_length(0x83),
            ReceiveLength::OtherInterrupt
        );
    }

    #[test]
    fn v1_interrupt_status_selects_the_vendor_byte_mode() {
        assert_eq!(
            RegisterMap::v1().receive_length(3),
            ReceiveLength::Blocks(3)
        );
        assert_eq!(
            RegisterMap::v1().receive_length(64),
            ReceiveLength::ByteMode
        );
        assert_eq!(
            RegisterMap::v1().receive_length(0x83),
            ReceiveLength::ByteMode
        );
    }

    #[test]
    fn v3_register_map_selects_v3_fifo_and_status_addresses() {
        let registers = RegisterMap::v3();
        assert_eq!(registers.block_count, 0x04);
        assert_eq!(registers.read_fifo, 0x0f);
        assert_eq!(registers.write_fifo, 0x10);
    }
}
