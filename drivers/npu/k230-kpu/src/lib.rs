#![no_std]

use core::{hint::spin_loop, ptr};

pub const KPU_CFG_PADDR: usize = 0x8040_0000;
pub const KPU_CFG_SIZE: usize = 0x800;
pub const KPU_L2_PADDR: usize = 0x8000_0000;
pub const KPU_L2_SIZE: usize = 0x20_0000;
pub const KPU_IRQ: usize = 189;

pub const COMMAND_START: usize = 0x100;
pub const COMMAND_END: usize = 0x104;
pub const COMMAND_HI: usize = 0x108;
pub const CONTROL: usize = 0x128;
pub const STATUS_LO: usize = 0x130;
pub const STATUS_HI: usize = 0x134;

pub const CONTROL_CLEAR: u32 = 0x4;
pub const CONTROL_START: u32 = 0x9;
pub const DONE_STATUS: u64 = 0x0000_0004_0000_0004;

pub const KPU_IOC_GET_STATUS: u32 = 0x4b00;
pub const KPU_IOC_CLEAR: u32 = 0x4b01;
pub const KPU_IOC_PROGRAM_COMMAND: u32 = 0x4b02;
pub const KPU_IOC_START: u32 = 0x4b03;
pub const KPU_IOC_RUN: u32 = 0x4b04;
pub const KPU_IOC_WAIT_DONE: u32 = 0x4b05;

pub const KPU_MMAP_CFG_OFFSET: u64 = 0;
pub const KPU_MMAP_L2_OFFSET: u64 = 0x1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    CommandRangeCrosses4G,
    CommandRangeEmpty,
    TimedOut,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandRange {
    pub start_paddr: u64,
    pub end_paddr: u64,
}

#[derive(Debug, Clone, Copy)]
pub struct Kpu {
    base_vaddr: usize,
}

impl Kpu {
    /// # Safety
    ///
    /// `base_vaddr` must point to a valid mapped K230 KPU CFG MMIO window.
    pub const unsafe fn new(base_vaddr: usize) -> Self {
        Self { base_vaddr }
    }

    pub fn program_command(&self, range: CommandRange) -> Result<(), Error> {
        let (start, end, hi) = command_words(range)?;
        self.write_reg(COMMAND_START, start);
        self.write_reg(COMMAND_END, end);
        self.write_reg(COMMAND_HI, hi);
        Ok(())
    }

    pub fn run_command(&self, range: CommandRange) -> Result<(), Error> {
        self.clear_done();
        self.program_command(range)?;
        self.start();
        Ok(())
    }

    pub fn clear_done(&self) {
        self.write_reg(CONTROL, CONTROL_CLEAR);
    }

    pub fn start(&self) {
        self.write_reg(CONTROL, CONTROL_START);
    }

    pub fn status(&self) -> u64 {
        let lo = self.read_reg(STATUS_LO) as u64;
        let hi = self.read_reg(STATUS_HI) as u64;
        (hi << 32) | lo
    }

    pub fn is_done(&self) -> bool {
        self.status() & DONE_STATUS == DONE_STATUS
    }

    pub fn wait_done(&self, poll_limit: usize) -> Result<(), Error> {
        for _ in 0..poll_limit {
            if self.is_done() {
                return Ok(());
            }
            spin_loop();
        }
        Err(Error::TimedOut)
    }

    pub fn read_reg(&self, offset: usize) -> u32 {
        unsafe { ptr::read_volatile((self.base_vaddr + offset) as *const u32) }
    }

    pub fn write_reg(&self, offset: usize, value: u32) {
        unsafe { ptr::write_volatile((self.base_vaddr + offset) as *mut u32, value) }
    }
}

pub fn command_words(range: CommandRange) -> Result<(u32, u32, u32), Error> {
    if range.start_paddr >= range.end_paddr {
        return Err(Error::CommandRangeEmpty);
    }
    if range.start_paddr >> 32 != range.end_paddr >> 32 {
        return Err(Error::CommandRangeCrosses4G);
    }
    Ok((
        range.start_paddr as u32,
        range.end_paddr as u32,
        (range.start_paddr >> 32) as u32,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_words_accept_same_4g_window() {
        assert_eq!(
            command_words(CommandRange {
                start_paddr: 0x1_0000_1000,
                end_paddr: 0x1_0000_2000,
            }),
            Ok((0x1000, 0x2000, 0x1))
        );
    }

    #[test]
    fn command_words_reject_empty_range() {
        assert_eq!(
            command_words(CommandRange {
                start_paddr: 0x1000,
                end_paddr: 0x1000,
            }),
            Err(Error::CommandRangeEmpty)
        );
    }

    #[test]
    fn command_words_reject_crossing_4g_window() {
        assert_eq!(
            command_words(CommandRange {
                start_paddr: 0xffff_ff00,
                end_paddr: 0x1_0000_0100,
            }),
            Err(Error::CommandRangeCrosses4G)
        );
    }
}
