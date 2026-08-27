//! AIC SDIO function-register layout.
//!
//! These are card function-space registers reached through CMD52/CMD53, not
//! processor MMIO. `tock-registers` still provides one checked definition of
//! every bitfield used by the protocol state machines.

use tock_registers::register_bitfields;

use crate::common::ChipVariant;

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
pub(crate) struct RegisterMap {
    pub block_count: u32,
    pub byte_mode_enable: u32,
    pub flow_control: u32,
    pub interrupt_enable: u32,
    pub read_fifo: u32,
    pub sleep_status: Option<u32>,
    pub wakeup: Option<u32>,
    pub write_fifo: u32,
}

impl RegisterMap {
    pub(crate) const fn for_chip(chip: ChipVariant) -> Self {
        if matches!(chip, ChipVariant::Aic8800D80 | ChipVariant::Aic8800D80X2) {
            Self {
                block_count: 0x04,
                byte_mode_enable: 0x07,
                flow_control: 0x03,
                interrupt_enable: 0x00,
                read_fifo: 0x0f,
                sleep_status: Some(0x01),
                wakeup: Some(0x02),
                write_fifo: 0x10,
            }
        } else {
            Self {
                block_count: 0x12,
                byte_mode_enable: 0x11,
                flow_control: 0x0a,
                interrupt_enable: 0x04,
                read_fifo: 0x08,
                sleep_status: None,
                wakeup: Some(0x09),
                write_fifo: 0x07,
            }
        }
    }
}

pub(crate) fn flow_credits(value: u8) -> u8 {
    (value & FLOW_CONTROL::CREDITS.mask) >> FLOW_CONTROL::CREDITS.shift
}

pub(crate) fn interrupt_block_count(value: u8) -> Option<u8> {
    if value & INTERRUPT_STATUS::OTHER::SET.value != 0 {
        None
    } else {
        Some((value & INTERRUPT_STATUS::BLOCK_COUNT.mask) >> INTERRUPT_STATUS::BLOCK_COUNT.shift)
    }
}

pub(crate) fn interface_ready(value: u8) -> bool {
    value & SLEEP_STATUS::READY::SET.value != 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interrupt_status_keeps_other_source_separate_from_block_count() {
        assert_eq!(interrupt_block_count(3), Some(3));
        assert_eq!(interrupt_block_count(0x83), None);
    }

    #[test]
    fn v3_register_map_selects_v3_fifo_and_status_addresses() {
        let registers = RegisterMap::for_chip(ChipVariant::Aic8800D80);
        assert_eq!(registers.block_count, 0x04);
        assert_eq!(registers.read_fifo, 0x0f);
        assert_eq!(registers.write_fifo, 0x10);
    }
}
