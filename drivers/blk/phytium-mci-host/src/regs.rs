//! Phytium MCI/FSDIF register definitions.

use bitfield_struct::bitfield;
use volatile::{VolatileFieldAccess, access::ReadOnly};

#[repr(C)]
#[derive(VolatileFieldAccess)]
pub struct RegisterBlock {
    pub ctrl: Ctrl,
    pub pwren: u32,
    pub clkdiv: u32,
    _reserved0: u32,
    pub clkena: ClkEna,
    pub tmout: u32,
    pub ctype: CType,
    pub blksiz: u32,
    pub bytcnt: u32,
    pub intmask: u32,
    pub cmdarg: u32,
    pub cmd: Cmd,
    #[access(ReadOnly)]
    pub resp: [u32; 4],
    #[access(ReadOnly)]
    pub mintsts: u32,
    pub rintsts: RIntSts,
    #[access(ReadOnly)]
    pub status: Status,
    pub fifoth: u32,
    #[access(ReadOnly)]
    pub cdetect: u32,
    #[access(ReadOnly)]
    pub wrtprt: u32,
    #[access(ReadOnly)]
    pub cksts: ClockStatus,
    #[access(ReadOnly)]
    pub tcbcnt: u32,
    #[access(ReadOnly)]
    pub tbbcnt: u32,
    pub debnce: u32,
    #[access(ReadOnly)]
    pub usrid: u32,
    #[access(ReadOnly)]
    pub verid: u32,
    #[access(ReadOnly)]
    pub hcon: u32,
    pub uhs: Uhs,
    pub rst: u32,
    _reserved1: u32,
    pub bmod: u32,
    pub pldmnd: u32,
    pub dbaddrl: u32,
    pub dbaddrh: u32,
    pub idsts: u32,
    pub idinten: u32,
    #[access(ReadOnly)]
    pub dscaddrl: u32,
    #[access(ReadOnly)]
    pub dscaddrh: u32,
    #[access(ReadOnly)]
    pub bufaddrl: u32,
    #[access(ReadOnly)]
    pub bufaddrh: u32,
}

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

#[bitfield(u32, order = Msb)]
pub struct ClkEna {
    pub cclk_low_power: u16,
    pub cclk_enable: u16,
}

#[bitfield(u32, order = Msb)]
pub struct CType {
    pub width8: u16,
    pub width4: u16,
}

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
    pub update_clock_registers_only: bool,
    #[bits(5)]
    pub card_number: u16,
    pub send_initialization: bool,
    pub stop_abort_cmd: bool,
    #[bits(default = true)]
    pub wait_prvdata_complete: bool,
    pub send_auto_stop: bool,
    pub transfer_mode: bool,
    pub read_write: bool,
    pub data_expected: bool,
    pub check_response_crc: bool,
    pub response_length: bool,
    pub response_expect: bool,
    #[bits(6)]
    pub cmd_index: u8,
}

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
    pub fn error(&self) -> bool {
        self.response_error()
            || self.response_crc_error()
            || self.data_crc_error()
            || self.response_timeout()
            || self.data_read_timeout()
            || self.host_timeout()
            || self.fifo_under_over_run()
            || self.hardware_locked_write()
            || self.start_bit_error()
            || self.end_bit_error()
    }
}

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

#[bitfield(u32, order = Msb)]
pub struct ClockStatus {
    #[bits(31)]
    __: u32,
    pub ready: bool,
}

#[bitfield(u32, order = Msb)]
pub struct Uhs {
    pub ddr: u16,
    pub volt: u16,
}

#[bitfield(u32, order = Msb)]
pub struct ClockSource {
    pub ext_clock_mux: bool,
    #[bits(7)]
    pub drive_phase: u8,
    __: bool,
    #[bits(7)]
    pub sample_phase: u8,
    __: bool,
    #[bits(7)]
    pub clock_div: u8,
    #[bits(6)]
    __: u8,
    pub ext_clock_enable: bool,
    pub ext_mmc_volt: bool,
}

pub const CARD_THRCTL_OFFSET: usize = 0x100;
pub const CLK_SRC_OFFSET: usize = 0x108;
