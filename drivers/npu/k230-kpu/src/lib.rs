#![no_std]

use core::{hint::spin_loop, ptr};

pub const KPU_CFG_PADDR: usize = 0x8040_0000;
pub const KPU_CFG_SIZE: usize = 0x800;
pub const KPU_L2_PADDR: usize = 0x8000_0000;
pub const KPU_L2_SIZE: usize = 0x20_0000;
pub const KPU_IRQ: usize = 189;
pub const KPU_FAKE_OUTPUT_PADDR: usize = 0x1009_0000;
pub const KPU_FAKE_OUTPUT_SIZE: usize = 0x10_0000;
pub const KPU_RUNTIME_RDATA_PADDR: usize = 0x1000_0000;
pub const KPU_RUNTIME_RDATA_SIZE: usize = 0x9_0000;
pub const KPU_RUNTIME_COMMAND_PADDR: usize = 0x1019_0000;
pub const KPU_RUNTIME_COMMAND_SIZE: usize = 0x37_0000;
pub const KPU_RUNTIME_DIRECT_IO_PADDR: usize = 0x1050_0000;
pub const KPU_RUNTIME_DIRECT_IO_SIZE: usize = 0xb0_0000;
pub const KPU_RUNTIME_DDR_PADDR: usize = 0x3c00_0000;
pub const KPU_RUNTIME_DDR_SIZE: usize = 0x400_0000;

pub const KPU_RUNTIME_RDATA_BASE: usize = 0x1000_0020;
pub const KPU_RUNTIME_FUNCTION_COMMAND_PADDR: usize = 0x1032_b020;
pub const KPU_RUNTIME_ARG_TABLE_PADDR: usize = KPU_L2_PADDR;
pub const KPU_RUNTIME_DIRECT_SOURCE_PADDR: usize = 0x1050_0020;
pub const KPU_RUNTIME_DIRECT_OUTPUT_PADDR: usize = 0x1050_1020;

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
pub const KPU_IOC_GET_INFO: u32 = 0x4b06;
pub const KPU_IOC_GET_IRQ_COUNT: u32 = 0x4b07;

pub const KPU_MMAP_CFG_OFFSET: u64 = 0;
pub const KPU_MMAP_L2_OFFSET: u64 = 0x1000;
pub const KPU_MMAP_FAKE_OUTPUT_OFFSET: u64 = 0x2000;
pub const KPU_MMAP_RUNTIME_RDATA_OFFSET: u64 = 0x3000;
pub const KPU_MMAP_RUNTIME_COMMAND_OFFSET: u64 = 0x4000;
pub const KPU_MMAP_RUNTIME_DIRECT_IO_OFFSET: u64 = 0x5000;
pub const KPU_MMAP_RUNTIME_DDR_OFFSET: u64 = 0x6000;

pub const KPU_IRQ_NONE: u32 = u32::MAX;
pub const KPU_INFO_F_FDT: u32 = 0x1;
pub const KPU_INFO_F_IRQ_WAIT: u32 = 0x2;
pub const KPU_INFO_F_FAKE_OUTPUT: u32 = 0x4;
pub const KPU_INFO_F_RUNTIME_SCRATCH: u32 = 0x8;

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

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KpuInfo {
    pub cfg_paddr: u64,
    pub cfg_size: u64,
    pub l2_paddr: u64,
    pub l2_size: u64,
    pub irq: u32,
    pub flags: u32,
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

    #[test]
    fn uapi_layout_and_constants_are_stable() {
        assert_eq!(core::mem::size_of::<CommandRange>(), 16);
        assert_eq!(core::mem::offset_of!(CommandRange, start_paddr), 0);
        assert_eq!(core::mem::offset_of!(CommandRange, end_paddr), 8);

        assert_eq!(KPU_IOC_GET_STATUS, 0x4b00);
        assert_eq!(KPU_IOC_CLEAR, 0x4b01);
        assert_eq!(KPU_IOC_PROGRAM_COMMAND, 0x4b02);
        assert_eq!(KPU_IOC_START, 0x4b03);
        assert_eq!(KPU_IOC_RUN, 0x4b04);
        assert_eq!(KPU_IOC_WAIT_DONE, 0x4b05);
        assert_eq!(KPU_IOC_GET_INFO, 0x4b06);
        assert_eq!(KPU_IOC_GET_IRQ_COUNT, 0x4b07);
        assert_eq!(KPU_MMAP_CFG_OFFSET, 0);
        assert_eq!(KPU_MMAP_L2_OFFSET, 0x1000);
        assert_eq!(KPU_MMAP_FAKE_OUTPUT_OFFSET, 0x2000);
        assert_eq!(KPU_MMAP_RUNTIME_RDATA_OFFSET, 0x3000);
        assert_eq!(KPU_MMAP_RUNTIME_COMMAND_OFFSET, 0x4000);
        assert_eq!(KPU_MMAP_RUNTIME_DIRECT_IO_OFFSET, 0x5000);
        assert_eq!(KPU_MMAP_RUNTIME_DDR_OFFSET, 0x6000);
        assert_eq!(KPU_CFG_PADDR, 0x8040_0000);
        assert_eq!(KPU_L2_PADDR, 0x8000_0000);
        assert_eq!(KPU_FAKE_OUTPUT_PADDR, 0x1009_0000);
        assert_eq!(KPU_RUNTIME_RDATA_PADDR, 0x1000_0000);
        assert_eq!(KPU_RUNTIME_COMMAND_PADDR, 0x1019_0000);
        assert_eq!(KPU_RUNTIME_DIRECT_IO_PADDR, 0x1050_0000);
        assert_eq!(KPU_RUNTIME_DDR_PADDR, 0x3c00_0000);
        assert_eq!(KPU_RUNTIME_RDATA_BASE, 0x1000_0020);
        assert_eq!(KPU_RUNTIME_FUNCTION_COMMAND_PADDR, 0x1032_b020);
        assert_eq!(KPU_RUNTIME_ARG_TABLE_PADDR, 0x8000_0000);
        assert_eq!(KPU_RUNTIME_DIRECT_SOURCE_PADDR, 0x1050_0020);
        assert_eq!(KPU_RUNTIME_DIRECT_OUTPUT_PADDR, 0x1050_1020);
        assert_eq!(KPU_CFG_SIZE, 0x800);
        assert_eq!(KPU_L2_SIZE, 0x20_0000);
        assert_eq!(KPU_FAKE_OUTPUT_SIZE, 0x10_0000);
        assert_eq!(KPU_RUNTIME_RDATA_SIZE, 0x9_0000);
        assert_eq!(KPU_RUNTIME_COMMAND_SIZE, 0x37_0000);
        assert_eq!(KPU_RUNTIME_DIRECT_IO_SIZE, 0xb0_0000);
        assert_eq!(KPU_RUNTIME_DDR_SIZE, 0x400_0000);
        assert_eq!(KPU_IRQ_NONE, u32::MAX);
        assert_eq!(KPU_INFO_F_FDT, 0x1);
        assert_eq!(KPU_INFO_F_IRQ_WAIT, 0x2);
        assert_eq!(KPU_INFO_F_FAKE_OUTPUT, 0x4);
        assert_eq!(KPU_INFO_F_RUNTIME_SCRATCH, 0x8);
        assert_eq!(COMMAND_START, 0x100);
        assert_eq!(COMMAND_END, 0x104);
        assert_eq!(COMMAND_HI, 0x108);
        assert_eq!(CONTROL, 0x128);
        assert_eq!(STATUS_LO, 0x130);
        assert_eq!(STATUS_HI, 0x134);
        assert_eq!(DONE_STATUS, 0x0000_0004_0000_0004);

        assert_eq!(core::mem::size_of::<KpuInfo>(), 40);
        assert_eq!(core::mem::offset_of!(KpuInfo, cfg_paddr), 0);
        assert_eq!(core::mem::offset_of!(KpuInfo, cfg_size), 8);
        assert_eq!(core::mem::offset_of!(KpuInfo, l2_paddr), 16);
        assert_eq!(core::mem::offset_of!(KpuInfo, l2_size), 24);
        assert_eq!(core::mem::offset_of!(KpuInfo, irq), 32);
        assert_eq!(core::mem::offset_of!(KpuInfo, flags), 36);
    }
}
