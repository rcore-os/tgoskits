//! Synopsys DesignWare Mobile Storage Host Controller (DW_mshc) register
//! definitions.
//!
//! Layout follows the publicly-available DesignWare DWC_mobile_storage
//! databook. Register names are kept identical to the databook so the
//! Linux `dw_mmc` driver and vendor SDK headers can be cross-referenced
//! without translation.
//!
//! The data FIFO is *not* part of [`RegisterBlock`]: different DW_mmc
//! variants place it at offset `0x100`, `0x200`, or `0x400`, and the
//! host driver accesses it through a raw pointer offset chosen at
//! construction time (see [`crate::host::DwMmc::new_with_fifo_offset`]).

use bitfield_struct::bitfield;
use volatile::{VolatileFieldAccess, access::ReadOnly};

/// DW_mshc register block (offsets 0x000..0x08C).
///
/// Volatile field accessors are derived; use them through the
/// [`VolatilePtr`](volatile::VolatilePtr) created in [`crate::host::DwMmc`].
#[repr(C)]
#[derive(VolatileFieldAccess)]
pub struct RegisterBlock {
    /// Control Register
    pub ctrl: Ctrl,
    /// Power Enable Register
    pub pwren: u32,
    /// Clock Divider Register
    pub clkdiv: ClkDiv,
    /// Clock Source Register (4 dividers, 16 cards)
    pub clksrc: u32,
    /// Clock Enable Register
    pub clkena: ClkEna,
    /// Timeout Register
    pub tmout: u32,
    /// Card Type Register (1-bit / 4-bit / 8-bit)
    pub ctype: CType,
    /// Block Size Register
    pub blksiz: BlkSiz,
    /// Byte Count Register — total bytes for the next data phase.
    pub bytcnt: u32,
    /// Interrupt Mask Register
    pub intmask: u32,
    /// Command Argument Register
    pub cmdarg: u32,
    /// Command Register
    pub cmd: Cmd,
    /// Response Register 0
    #[access(ReadOnly)]
    pub resp0: u32,
    /// Response Register 1
    #[access(ReadOnly)]
    pub resp1: u32,
    /// Response Register 2
    #[access(ReadOnly)]
    pub resp2: u32,
    /// Response Register 3
    #[access(ReadOnly)]
    pub resp3: u32,
    /// Masked Interrupt Status Register
    #[access(ReadOnly)]
    pub mintsts: u32,
    /// Raw Interrupt Status Register (write-1-to-clear)
    pub rintsts: RIntSts,
    /// Status Register
    #[access(ReadOnly)]
    pub status: Status,
    /// FIFO Threshold Watermark Register
    pub fifoth: u32,
    /// Card Detect Register
    #[access(ReadOnly)]
    pub cdetect: u32,
    /// Write Protect Register
    #[access(ReadOnly)]
    pub wrtprt: u32,
    /// General Purpose Input/Output Register
    pub gpio: u32,
    /// Transferred CIU Card Byte Count
    #[access(ReadOnly)]
    pub tcbcnt: u32,
    /// Transferred Host to BIU-FIFO Byte Count
    #[access(ReadOnly)]
    pub tbbcnt: u32,
    /// Debounce Count Register
    pub debnce: u32,
    /// User ID Register (scratch)
    #[access(ReadOnly)]
    pub usrid: u32,
    /// Synopsys Version ID Register
    #[access(ReadOnly)]
    pub verid: u32,
    /// Hardware Configuration Register
    #[access(ReadOnly)]
    pub hcon: u32,
    /// UHS-1 Register (DDR mode + 1.8 V signaling)
    pub uhs: UHS,
    /// Hardware Reset Register (per-card reset lines)
    pub rst: u32,
    _reserved0: u32,
    /// Bus Mode Register (IDMAC enable + reset)
    pub bmod: u32,
    /// Poll Demand Register (write-only)
    pub pldmnd: u32,
    /// Descriptor List Base Address Register
    pub dbaddr: u32,
    /// Internal DMAC Status Register (write-1-to-clear).
    pub idsts: u32,
    /// Internal DMAC Interrupt Enable Register.
    pub idinten: u32,
}

/// Control Register
#[bitfield(u32, order = Msb)]
pub struct Ctrl {
    #[bits(6)]
    __: u8,
    pub use_internal_dmac: bool,
    pub enable_od_pullup: bool,
    #[bits(4)]
    pub card_voltage_b: u8,
    #[bits(4)]
    pub card_voltage_a: u8,
    #[bits(4)]
    __: u8,
    pub ceata_device_interrupt: bool,
    pub send_auto_stop_ccsd: bool,
    pub send_ccsd: bool,
    pub abort_read_data: bool,
    pub send_irq_response: bool,
    pub read_wait: bool,
    pub dma_enable: bool,
    pub int_enable: bool,
    __: bool,
    pub dma_reset: bool,
    pub fifo_reset: bool,
    pub controller_reset: bool,
}

/// Clock Divider Register (four 8-bit dividers; only divider-0 used here).
#[bitfield(u32, order = Msb)]
pub struct ClkDiv {
    pub clk_divider3: u8,
    pub clk_divider2: u8,
    pub clk_divider1: u8,
    pub clk_divider0: u8,
}

/// Clock Enable Register
#[bitfield(u32, order = Msb)]
pub struct ClkEna {
    pub cclk_low_power: u16,
    pub cclk_enable: u16,
}

/// Card Type Register (per-card bus-width selection).
#[bitfield(u32, order = Msb)]
pub struct CType {
    /// 1 = card is 8-bit. Per card.
    pub width8: u16,
    /// 1 = card is 4-bit (else 1-bit). Per card.
    pub width4: u16,
}

/// Block Size Register (lower 16 bits hold the block length in bytes).
#[bitfield(u32, order = Msb)]
pub struct BlkSiz {
    __: u16,
    pub block_size: u16,
}

/// Command Register.
///
/// Writing this register with [`Cmd::start_cmd`] = 1 hands the encoded
/// command to the CIU; the bit auto-clears when the controller has
/// accepted it. The CIU then drives CMD/DAT lines and raises
/// [`RIntSts::command_done`] (and, for data phases,
/// [`RIntSts::data_transfer_over`]).
#[bitfield(u32, order = Msb)]
pub struct Cmd {
    #[bits(default = true)]
    pub start_cmd: bool,
    __: bool,
    pub use_hold_reg: bool,
    pub volt_switch: bool,
    pub boot_mode: bool,
    pub disable_boot: bool,
    pub expect_boot_ack: bool,
    pub enable_boot: bool,
    pub ccs_expected: bool,
    pub read_ceata_device: bool,
    /// 1 = update clock-related registers without sending a command.
    /// Required after every CLKDIV / CLKENA / CLKSRC change.
    pub update_clock_registers_only: bool,
    #[bits(5)]
    pub card_number: u16,
    pub send_initialization: bool,
    pub stop_abort_cmd: bool,
    #[bits(default = true)]
    pub wait_prvdata_complete: bool,
    pub send_auto_stop: bool,
    pub transfer_mode: bool,
    /// 0 = read from card, 1 = write to card.
    pub read_write: bool,
    pub data_expected: bool,
    pub check_response_crc: bool,
    /// 0 = short response, 1 = long (R2) response.
    pub response_length: bool,
    pub response_expect: bool,
    #[bits(6)]
    pub cmd_index: u8,
}

/// Raw Interrupt Status Register. Write-1-to-clear.
#[bitfield(u32, order = Msb)]
pub struct RIntSts {
    pub sdio: u16,
    pub end_bit_error: bool,
    pub auto_command_done: bool,
    pub start_bit_error: bool,
    pub hardware_locked_write: bool,
    pub fifo_under_over_run: bool,
    pub host_timeout: bool,
    pub data_read_timeout: bool,
    pub response_timeout: bool,
    pub data_crc_error: bool,
    pub response_crc_error: bool,
    pub receive_fifo_data_request: bool,
    pub transmit_fifo_data_request: bool,
    pub data_transfer_over: bool,
    pub command_done: bool,
    pub response_error: bool,
    pub card_detect: bool,
}

impl RIntSts {
    /// True when any error / timeout / CRC bit is asserted.
    pub fn error(&self) -> bool {
        self.response_timeout()
            || self.data_read_timeout()
            || self.host_timeout()
            || self.fifo_under_over_run()
            || self.start_bit_error()
            || self.end_bit_error()
            || self.data_crc_error()
            || self.response_crc_error()
            || self.response_error()
            || self.hardware_locked_write()
    }
}

/// Status Register.
#[bitfield(u32, order = Msb)]
pub struct Status {
    pub dma_req: bool,
    pub dma_ack: bool,
    #[bits(13)]
    pub fifo_count: u16,
    #[bits(6)]
    pub response_index: u8,
    pub data_state_mc_busy: bool,
    pub data_busy: bool,
    pub data_3_status: bool,
    #[bits(4)]
    pub command_fsm_states: u8,
    pub fifo_full: bool,
    pub fifo_empty: bool,
    pub fifo_tx_watermark: bool,
    pub fifo_rx_watermark: bool,
}

/// UHS-1 Register: DDR mode + 1.8 V signaling, per card.
#[bitfield(u32, order = Msb)]
pub struct UHS {
    pub ddr: u16,
    /// Per card: 0 = 3.3 V buffers, 1 = 1.8 V buffers.
    pub volt: u16,
}

#[cfg(test)]
mod tests {
    use core::mem::{offset_of, size_of};

    use super::*;

    #[test]
    fn response_registers_are_individual_32_bit_mmio_words() {
        assert_eq!(offset_of!(RegisterBlock, resp0), 0x30);
        assert_eq!(offset_of!(RegisterBlock, resp1), 0x34);
        assert_eq!(offset_of!(RegisterBlock, resp2), 0x38);
        assert_eq!(offset_of!(RegisterBlock, resp3), 0x3c);
        assert_eq!(
            offset_of!(RegisterBlock, mintsts) - offset_of!(RegisterBlock, resp3),
            size_of::<u32>()
        );
    }
}
