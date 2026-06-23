//! RGA2 hardware backend (legacy 32-word command block, local-MMU-capable; PR-1 uses MMU-off).
pub mod command;
pub mod registers;

use core::ptr::NonNull;

use dma_api::{DeviceDma, DmaDirection};

use crate::{
    RgaHardwareVersion, RgaVersion,
    backend::{RgaBackend, RgaDiag, RgaStatus},
    buffer::RgaDmaBuffer,
    error::{Result, RgaError},
    operation::RgaOperation,
};

// Register bit values from the RK3588 TRM Part2 RGA2 chapter (cross-checked vs the vendor rga2 driver).
const CMD_CTRL_START: u32 = 1 << 0; // CMD_CTRL.sw_cmd_line_st_p — start command fetch (auto-clears)
// INT (0x0010): RO status flags in bits[3:0]; dedicated W1C clear bits in bits[7:4].
const INT_DONE: u32 = 1 << 2; // sw_intr_af — all-command-finished ("done")
const INT_ERROR: u32 = 1 << 0; // sw_intr_err — error
const INT_DONE_CLR: u32 = 1 << 6; // sw_intr_af_clr (W1C)
const INT_ERROR_CLR: u32 = 1 << 4; // sw_intr_err_clr (W1C)
// SYS_CTRL (0x0000): soft-reset = aclk-domain (bit3) | core-clk-domain (bit4). (bit0 is command-start, NOT reset.)
const SYS_CTRL_SOFT_RESET: u32 = (1 << 3) | (1 << 4);
// SYS_CTRL run/mode bits. Value 0x66 is the vendor rga2_drv.c convention (RK3288-family TRM);
// NOT re-verified against the RK3588 TRM in-repo — the on-timeout diag reads SYS_CTRL back to
// confirm. bit1 sw_cmd_mode=1 (MASTER/command-list, so CMD_CTRL.start drives the command-DMA
// fetch), bit2 sw_auto_ckg, bit5 sw_auto_rst, bit6. Without bit1 the core stays in slave mode
// and CMD_CTRL.start fetches nothing.
const SYS_CTRL_RUN: u32 = (1 << 1) | (1 << 2) | (1 << 5) | (1 << 6);

/// RGA2 core controller. Owns its MMIO region and a lazily-allocated DMA command buffer.
pub struct Rga2Backend {
    base: NonNull<u8>,
    dma: DeviceDma,
    cmd: Option<RgaDmaBuffer>,
}

// SAFETY: `base` is an MMIO region owned by this backend; access is serialized through `&mut self`.
unsafe impl Send for Rga2Backend {}

impl Rga2Backend {
    pub fn new(base: NonNull<u8>, dma: DeviceDma) -> Self {
        Self {
            base,
            dma,
            cmd: None,
        }
    }

    fn write32(&self, off: usize, val: u32) {
        // SAFETY: `off` is a valid in-range RGA2 register offset; `base` is a mapped MMIO region.
        unsafe {
            self.base
                .as_ptr()
                .add(off)
                .cast::<u32>()
                .write_volatile(val)
        }
    }

    fn read32(&self, off: usize) -> u32 {
        // SAFETY: as above.
        unsafe { self.base.as_ptr().add(off).cast::<u32>().read_volatile() }
    }
}

impl RgaBackend for Rga2Backend {
    fn generation(&self) -> RgaVersion {
        RgaVersion::Rga2
    }

    fn read_version(&self) -> RgaHardwareVersion {
        let raw = self.read32(registers::VERSION_INFO);
        RgaHardwareVersion {
            raw,
            major: ((raw >> 24) & 0xff) as u8,
            minor: ((raw >> 20) & 0x0f) as u8,
        }
    }

    fn supports(&self, _op: &RgaOperation) -> Result<()> {
        // Validation (incl. Blit geometry/format/CSC) is enforced upstream by op.validate();
        // RGA2 accepts all validated ops.
        Ok(())
    }

    fn submit(&mut self, op: &RgaOperation) -> Result<()> {
        op.validate()?;
        let words = command::encode(op)?;
        if self.cmd.is_none() {
            self.cmd = Some(RgaDmaBuffer::alloc(
                &self.dma,
                registers::CMD_BUFFER_WORDS * 4,
                DmaDirection::ToDevice,
            )?);
        }
        let cmd = self.cmd.as_mut().ok_or(RgaError::Dma)?;
        // SAFETY: the slice is not retained across the device submission below.
        let bytes = unsafe { cmd.cpu_bytes_mut() };
        for (i, w) in words.words().iter().enumerate() {
            bytes[i * 4..i * 4 + 4].copy_from_slice(&w.to_le_bytes());
        }
        cmd.prepare_for_device();
        let cmd_phys = cmd.phys_addr();

        self.write32(registers::INT, INT_DONE_CLR | INT_ERROR_CLR); // W1C clear bits[7:4]
        // Put the core into MASTER/command-list mode so CMD_CTRL.start drives a command-DMA fetch
        // (vendor rga2_drv.c writes SYS_CTRL=0x66 between CMD_BASE and CMD_CTRL; slave mode = no exec).
        self.write32(registers::SYS_CTRL, 0);
        // CMD_BASE (sw_cmd_base[31:0]) is a raw byte address — confirmed against the vendor rga2 driver:
        // `rga2_write(virt_to_phys(cmd_buff), RGA2_CMD_BASE)`, no shift. (The >>4 belongs to MMU_*_BASE,
        // the page-table base used only on the MMU-on path.) Command buffer is 128B-aligned.
        self.write32(registers::CMD_BASE, cmd_phys as u32);
        self.write32(registers::SYS_CTRL, SYS_CTRL_RUN);
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        self.write32(registers::CMD_CTRL, CMD_CTRL_START);
        Ok(())
    }

    fn poll(&self) -> RgaStatus {
        let int = self.read32(registers::INT);
        if int & INT_ERROR != 0 {
            RgaStatus::Error
        } else if int & INT_DONE != 0 {
            RgaStatus::Done
        } else {
            RgaStatus::Busy
        }
    }

    fn diag(&self) -> RgaDiag {
        RgaDiag {
            int: self.read32(registers::INT),
            sys_ctrl: self.read32(registers::SYS_CTRL),
            cmd_ctrl: self.read32(registers::CMD_CTRL),
            cmd_base: self.read32(registers::CMD_BASE),
            status: self.read32(registers::STATUS),
            version: self.read32(registers::VERSION_INFO),
            cmd_phys: self.cmd.as_ref().map(|c| c.phys_addr()).unwrap_or(0),
        }
    }

    fn ack(&mut self) {
        self.write32(registers::INT, INT_DONE_CLR | INT_ERROR_CLR); // W1C clear bits[7:4]
    }

    fn reset(&mut self) -> Result<()> {
        self.write32(registers::SYS_CTRL, SYS_CTRL_SOFT_RESET);
        self.write32(registers::SYS_CTRL, 0);
        self.write32(registers::INT, INT_DONE_CLR | INT_ERROR_CLR); // W1C clear bits[7:4]
        Ok(())
    }
}
