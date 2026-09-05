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
                    // Vendor D80 IRQ handler composite encoding: the status
                    // byte folds the function-2 queue sentinel into bit 3.
                    // 120 (and 127) are byte-mode markers, 113..118 and
                    // 121..126 belong to the function-2 queue (1..6 blocks),
                    // and only 1..112 are plain function-1 block counts.
                    let function_two = value | (1 << 3);
                    if function_two > 120 {
                        if function_two == 127 {
                            ReceiveLength::ByteMode
                        } else {
                            ReceiveLength::Blocks(value & 0x07)
                        }
                    } else if value == 120 {
                        ReceiveLength::ByteMode
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
}

impl RegisterMap {
    /// Flow-credit count carried by the flow-control register.
    ///
    /// The vendor driver masks the register for 8801/DC/DW but reads the raw
    /// byte for D80; the V3 register is a plain eight-bit buffer count, so a
    /// drained mailbox reports 128 (0x80), which a seven-bit mask would read
    /// as zero and stall the TX path forever.
    pub(crate) fn flow_credits(self, value: u8) -> u8 {
        match self.receive_length_encoding {
            ReceiveLengthEncoding::V3 => value,
            ReceiveLengthEncoding::V1 => {
                (value & FLOW_CONTROL::CREDITS.mask) >> FLOW_CONTROL::CREDITS.shift
            }
        }
    }
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
    fn v3_interrupt_status_treats_120_as_the_byte_mode_marker() {
        assert_eq!(
            RegisterMap::v3().receive_length(120),
            ReceiveLength::ByteMode
        );
        assert_eq!(
            RegisterMap::v3().receive_length(119),
            ReceiveLength::ByteMode
        );
        assert_eq!(
            RegisterMap::v3().receive_length(127),
            ReceiveLength::ByteMode
        );
    }

    #[test]
    fn v3_interrupt_status_decodes_the_function_two_queue_domain() {
        // 113..118 and 121..126 fold the function-2 queue sentinel into bit 3;
        // they carry 1..6 blocks, not the raw value as a block count.
        assert_eq!(
            RegisterMap::v3().receive_length(113),
            ReceiveLength::Blocks(1)
        );
        assert_eq!(
            RegisterMap::v3().receive_length(118),
            ReceiveLength::Blocks(6)
        );
        assert_eq!(
            RegisterMap::v3().receive_length(121),
            ReceiveLength::Blocks(1)
        );
        assert_eq!(
            RegisterMap::v3().receive_length(126),
            ReceiveLength::Blocks(6)
        );
        // Plain function-1 block counts stay intact.
        assert_eq!(
            RegisterMap::v3().receive_length(112),
            ReceiveLength::Blocks(112)
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

    #[test]
    fn v3_flow_credits_read_the_full_byte_without_the_vendor_mask() {
        // The vendor applies the 7-bit mask only to 8801/DC/DW; the D80
        // register is a raw eight-bit count, and a drained mailbox reports
        // 128 (0x80).
        assert_eq!(RegisterMap::v3().flow_credits(0x80), 128);
        assert_eq!(RegisterMap::v3().flow_credits(0x85), 133);
        assert_eq!(RegisterMap::v1().flow_credits(0x80), 0);
        assert_eq!(RegisterMap::v1().flow_credits(0x85), 5);
    }
}
