use core::ptr::NonNull;

use log::{debug, info, trace};
use volatile::VolatilePtr;

use crate::{
    cmd::{Command, DataXfer},
    regs::{CType, ClkDiv, ClkEna, Cmd, RegisterBlock, RegisterBlockVolatileFieldAccess},
    utils::{Cid, CsdV2},
};

fn wait_until<F>(mut f: F)
where
    F: FnMut() -> bool,
{
    // TODO: yield?
    while !f() {
        core::hint::spin_loop();
    }
}

fn scr_bus_widths(scr: &[u8; 8]) -> u64 {
    let scr = u64::from_le_bytes(*scr);
    (scr >> 8) & 0xf
}

/// SD/MMC driver.
pub struct SdMmc {
    regs: VolatilePtr<'static, RegisterBlock>,
    num_blocks: u64,
    rca: u32,
}

impl SdMmc {
    const FIFO: usize = 0x200;

    /// Creates a new `SdMmc` instance from the given base address.
    ///
    /// # Safety
    ///
    /// The caller must ensure that `base` is a valid pointer to the SD/MMC controller's
    /// register block and that no other code is concurrently accessing the same hardware.
    pub unsafe fn new(base: usize) -> Self {
        let regs = unsafe { VolatilePtr::new(NonNull::new_unchecked(base as *mut _)) };

        let mut this = Self {
            regs,
            num_blocks: 0,
            rca: 0,
        };
        this.init();
        this
    }

    fn can_send_cmd(&self) -> bool {
        !self.regs.cmd().read().start_cmd()
    }

    fn can_send_data(&self) -> bool {
        !self.regs.status().read().data_busy()
    }

    fn has_response(&self) -> bool {
        self.regs.rintsts().read().command_done()
    }

    fn fifo_cnt(&self) -> usize {
        self.regs.status().read().fifo_count() as usize
    }

    fn set_transaction_size(&self, blk_size: u16, byte_cnt: u32) {
        self.regs.blksiz().update(|r| r.with_block_size(blk_size));
        self.regs.bytcnt().write(byte_cnt);
    }

    fn send_cmd(&self, command: Command<'_>) -> Option<[u32; 4]> {
        trace!("send_cmd {command:#x?}");

        let (cmd, arg, xfer) = command.build();
        assert_eq!(cmd.data_expected(), xfer.is_some());

        trace!("send_cmd {cmd:?} {arg:#x?}");

        wait_until(|| self.can_send_cmd());
        if cmd.data_expected() {
            wait_until(|| self.can_send_data());
        }

        self.regs.cmdarg().write(arg);
        self.regs.cmd().write(cmd);

        wait_until(|| self.can_send_cmd());
        trace!("cmd {} sent", cmd.cmd_index());

        if cmd.response_expect() {
            wait_until(|| self.has_response());
            trace!("cmd {} received response", cmd.cmd_index());
        }

        if let Some(xfer) = xfer {
            let fifo_base = unsafe { self.regs.as_raw_ptr().byte_add(Self::FIFO) }.cast::<u64>();
            let mut offset = 0;
            match xfer {
                DataXfer::Read(buf) => {
                    wait_until(|| {
                        let rintsts = self.regs.rintsts().read();

                        if rintsts.receive_fifo_data_request() {
                            trace!("rxdr");
                            while self.fifo_cnt() >= 2 {
                                let data = unsafe { fifo_base.read_volatile() };
                                if offset + 8 <= buf.len() {
                                    buf[offset..offset + 8].copy_from_slice(&data.to_le_bytes());
                                }
                                offset += 8;
                            }
                        }

                        rintsts.data_transfer_over() || rintsts.error()
                    });
                    trace!("received {offset} bytes");
                }
                DataXfer::Write(buf) => {
                    wait_until(|| {
                        let rintsts = self.regs.rintsts().read();

                        if rintsts.transmit_fifo_data_request() {
                            trace!("txdr");
                            while self.fifo_cnt() < 120 && offset < buf.len() {
                                let data =
                                    u64::from_le_bytes(buf[offset..offset + 8].try_into().unwrap());
                                unsafe { fifo_base.write_volatile(data) };
                                offset += 8;
                            }
                        }

                        rintsts.data_transfer_over() || rintsts.error()
                    });
                    trace!("sent {offset} bytes");
                }
            }
        }

        let resp = self.regs.resp().read();

        let rintsts = self.regs.rintsts().read();
        // clear interrupt status
        self.regs.rintsts().write(rintsts);

        if rintsts.error() {
            trace!("cmd {} error: {rintsts:?} resp: {resp:?}", cmd.cmd_index());
            return None;
        }
        Some(resp)
    }

    fn set_clock(&self, divider: u8) {
        self.regs.clkena().write(ClkEna::new());
        self.send_cmd(Command::ResetClock);

        self.regs
            .clkdiv()
            .write(ClkDiv::new().with_clk_divider0(divider));

        self.regs
            .clkena()
            .write(ClkEna::new().with_cclk_enable(1).with_cclk_low_power(0));
        self.send_cmd(Command::ResetClock);
    }

    fn init(&mut self) {
        info!("Initializing SD/MMC driver at {:?}", self.regs);

        trace!("ctrl: {:?}", self.regs.ctrl().read());
        trace!("pwren: {:?}", self.regs.pwren().read());
        trace!("clkdiv: {:?}", self.regs.clkdiv().read());
        trace!("clksrc: {:?}", self.regs.clksrc().read());
        trace!("clkena: {:?}", self.regs.clkena().read());
        trace!("tmout: {:?}", self.regs.tmout().read());
        trace!("ctype: {:?}", self.regs.ctype().read());
        trace!("cdetect: {:?}", self.regs.cdetect().read());
        trace!("wrtprt: {:?}", self.regs.wrtprt().read());
        trace!("usrid: {:?}", self.regs.usrid().read());
        trace!("verid: {:?}", self.regs.verid().read());
        trace!("hcon: {:?}", self.regs.hcon().read());
        trace!("uhs: {:?}", self.regs.uhs().read());
        trace!("bmod: {:?}", self.regs.bmod().read());
        trace!("dbaddr: {:?}", self.regs.dbaddr().read());

        // reset clock to identification frequency (400kHz)
        self.set_clock(4);

        // set data width -> 1bit
        self.regs.ctype().write(0.into());

        // reset dma
        self.regs.bmod().update(|r| r.with_de(false).with_swr(true));
        self.regs
            .ctrl()
            .update(|r| r.with_dma_reset(true).with_use_internal_dmac(false));

        trace!("dma reset");

        trace!("ctrl: {:?}", self.regs.ctrl().read());

        self.send_cmd(Command::GoIdleState);
        trace!("idle state set");

        let resp = self.send_cmd(Command::SendIfCond(0x1aa)).unwrap();
        assert_eq!(resp[0] & 0xff, 0xaa, "unsupported version");

        loop {
            self.send_cmd(Command::AppCmd(0));
            let resp = self.send_cmd(Command::SdSendOpCond(0x41FF_8000)).unwrap();
            let ocr = resp[0];
            if ocr & 0x8000_0000 != 0 {
                info!("SD card is ready");
                if ocr & 0x4000_0000 != 0 {
                    debug!("SD card supports high capacity");
                } else {
                    debug!("SD card is standard capacity");
                }
                break;
            }

            trace!("SD card not ready, ocr: {ocr:x}");
            core::hint::spin_loop();
        }

        let resp = self.send_cmd(Command::AllSendCid).unwrap();
        let cid = unsafe { core::mem::transmute::<[u32; 4], Cid>(resp) };
        info!("cid: {cid:?}");

        let resp = self.send_cmd(Command::SendRelativeAddr).unwrap();
        self.rca = (resp[0] >> 16) & 0xffff;
        debug!("rca: {:#x}", self.rca);

        let resp = self.send_cmd(Command::SendCsd(self.rca << 16)).unwrap();
        let csd = unsafe { core::mem::transmute::<[u32; 4], CsdV2>(resp) };
        debug!("csd: {csd:?}");

        self.num_blocks = csd.num_blocks();
        info!("SD card capacity: {:#x} blocks", self.num_blocks);

        self.send_cmd(Command::SelectCard(self.rca << 16)).unwrap();

        self.send_cmd(Command::AppCmd(self.rca << 16)).unwrap();

        self.set_transaction_size(8, 8);
        let mut buf = [0u8; 512];
        self.send_cmd(Command::SendScr(&mut buf)).unwrap();

        let bus_widths = scr_bus_widths(buf[..8].try_into().unwrap());
        debug!("Bus width supported: {:#x?}", bus_widths);

        trace!("fifo count: {}", self.fifo_cnt());

        trace!("ctrl: {:?}", self.regs.ctrl().read());
        let rintsts = self.regs.rintsts().read();
        trace!("rintsts: {rintsts:?}");
        self.regs.rintsts().write(rintsts); // clear interrupt status

        // Switch to 4-bit bus width if supported
        if bus_widths & 0x4 != 0 {
            self.send_cmd(Command::AppCmd(self.rca << 16)).unwrap();
            self.send_cmd(Command::SetBusWidth(0x2)).unwrap();
            self.regs.ctype().write(CType::new().with_width4(1));
            info!("Switched to 4-bit bus width");
        }

        // Increase clock speed for data transfer
        // Use divider 0 (bypass) — assumes CRU has already set a reasonable source clock.
        // Adjust this value based on your SoC's CCLK_SDMMC frequency.
        self.set_clock(0);
        info!("Switched to high-speed clock (clkdiv=0)");

        info!("SD/MMC driver initialized");
    }

    /// Reads a single block from the SD/MMC card.
    pub fn read_block(&mut self, block: u32, buf: &mut [u8; 512]) {
        self.set_transaction_size(512, 512);
        self.send_cmd(Command::ReadSingleBlock(block, buf)).unwrap();
        trace!("fifo count: {}", self.fifo_cnt());
    }

    /// Writes a single block to the SD/MMC card.
    pub fn write_block(&mut self, block: u32, buf: &[u8; 512]) {
        self.set_transaction_size(512, 512);
        self.send_cmd(Command::WriteSingleBlock(block, buf))
            .unwrap();
        trace!("fifo count: {}", self.fifo_cnt());
    }

    /// Reads multiple blocks from the SD/MMC card.
    pub fn read_blocks(&mut self, start_block: u32, buf: &mut [u8]) {
        let block_count = buf.len() / Self::BLOCK_SIZE;
        let byte_count = (block_count * Self::BLOCK_SIZE) as u32;
        self.set_transaction_size(512, byte_count);
        self.send_cmd(Command::ReadMultipleBlock(start_block, buf))
            .unwrap();
    }

    /// Writes multiple blocks to the SD/MMC card.
    pub fn write_blocks(&mut self, start_block: u32, buf: &[u8]) {
        let block_count = buf.len() / Self::BLOCK_SIZE;
        let byte_count = (block_count * Self::BLOCK_SIZE) as u32;
        self.set_transaction_size(512, byte_count);
        self.send_cmd(Command::WriteMultipleBlock(start_block, buf))
            .unwrap();
    }

    /// Reads multiple blocks using IDMAC (Internal DMA Controller).
    ///
    /// Assumes flat memory mapping: virtual address == physical address.
    /// The buffer must be DMA-coherent (no CPU cache issues).
    pub fn read_blocks_dma(&mut self, start_block: u32, buf: &mut [u8]) {
        let byte_count = (buf.len() / Self::BLOCK_SIZE * Self::BLOCK_SIZE) as u32;
        self.dma_transfer(start_block, byte_count, buf.as_ptr() as u32, true);
    }

    /// Writes multiple blocks using IDMAC (Internal DMA Controller).
    pub fn write_blocks_dma(&mut self, start_block: u32, buf: &[u8]) {
        let byte_count = (buf.len() / Self::BLOCK_SIZE * Self::BLOCK_SIZE) as u32;
        self.dma_transfer(start_block, byte_count, buf.as_ptr() as u32, false);
    }

    fn dma_transfer(&self, start_block: u32, byte_count: u32, buf_addr: u32, is_read: bool) {
        #[repr(C, align(16))]
        struct Desc {
            des0: u32,
            des1: u32,
            des2: u32,
            des3: u32,
        }
        const OWN: u32 = 1 << 31;
        const FS: u32 = 1 << 3; // First Descriptor
        const LD: u32 = 1 << 2; // Last Descriptor
        const DIC: u32 = 1 << 1; // Disable Interrupt on Completion

        let desc = Desc {
            des0: OWN | FS | LD | DIC,
            des1: byte_count,
            des2: buf_addr,
            des3: 0,
        };
        let desc_addr = &desc as *const Desc as u32;

        self.set_transaction_size(512, byte_count);

        // Push write data and descriptor from CPU cache to physical memory
        #[cfg(target_arch = "aarch64")]
        {
            if !is_read {
                clean_dcache_range(buf_addr as usize, byte_count as usize);
            }
            clean_dcache_range(desc_addr as usize, core::mem::size_of::<Desc>());
        }

        // Clear interrupts
        let rintsts = self.regs.rintsts().read();
        self.regs.rintsts().write(rintsts);

        // Set descriptor address
        self.regs.dbaddr().write(desc_addr);

        // Enable internal DMAC + IDMAC
        self.regs.ctrl().update(|r| r.with_use_internal_dmac(true));
        self.regs.bmod().update(|r| r.with_fb(true).with_de(true));

        // Build command: CMD18 (read) or CMD25 (write) with auto-stop
        let cmd = Cmd::default()
            .with_use_hold_reg(true)
            .with_response_expect(true)
            .with_check_response_crc(true)
            .with_data_expected(true)
            .with_send_auto_stop(true)
            .with_cmd_index(if is_read { 18 } else { 25 })
            .with_read_write(!is_read);

        wait_until(|| self.can_send_cmd());
        wait_until(|| self.can_send_data());

        self.regs.cmdarg().write(start_block);
        self.regs.cmd().write(cmd);

        // Wait for command done
        wait_until(|| self.can_send_cmd());
        wait_until(|| self.has_response());

        // Wait for DMA data transfer to complete
        wait_until(|| {
            let rintsts = self.regs.rintsts().read();
            rintsts.data_transfer_over() || rintsts.error()
        });

        // Invalidate read buffer in CPU cache (discard stale data)
        #[cfg(target_arch = "aarch64")]
        {
            if is_read {
                invalidate_dcache_range(buf_addr as usize, byte_count as usize);
            }
        }

        // Disable IDMAC
        self.regs.bmod().update(|r| r.with_de(false));
        self.regs.ctrl().update(|r| r.with_use_internal_dmac(false));

        // Clear interrupts
        let rintsts = self.regs.rintsts().read();
        self.regs.rintsts().write(rintsts);
    }

    /// Returns the number of blocks.
    pub fn num_blocks(&self) -> u64 {
        self.num_blocks
    }

    /// The size of a block in bytes.
    pub const BLOCK_SIZE: usize = 512;
}

unsafe impl Send for SdMmc {}
unsafe impl Sync for SdMmc {}

#[cfg(target_arch = "aarch64")]
fn clean_dcache_range(addr: usize, len: usize) {
    const LINE: usize = 64;
    let start = addr & !(LINE - 1);
    let end = (addr + len + LINE - 1) & !(LINE - 1);
    unsafe {
        for a in (start..end).step_by(LINE) {
            core::arch::asm!("dc cvau, {0}", in(reg) a);
        }
        core::arch::asm!("dsb sy");
    }
}

#[cfg(target_arch = "aarch64")]
fn invalidate_dcache_range(addr: usize, len: usize) {
    const LINE: usize = 64;
    let start = addr & !(LINE - 1);
    let end = (addr + len + LINE - 1) & !(LINE - 1);
    unsafe {
        for a in (start..end).step_by(LINE) {
            core::arch::asm!("dc ivac, {0}", in(reg) a);
        }
        core::arch::asm!("dsb sy");
    }
}
