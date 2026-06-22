//! RGA2 hardware backend (legacy 32-word command block, local-MMU-capable; PR-1 uses MMU-off).
pub mod command;
pub mod registers;

use core::ptr::NonNull;

use dma_api::{DeviceDma, DmaDirection};

use crate::{
    RgaHardwareVersion, RgaVersion,
    backend::{RgaBackend, RgaStatus},
    buffer::RgaDmaBuffer,
    error::{Result, RgaError},
    operation::RgaOperation,
};

// CONFIRM ON BOARD (Task 13): these bit values are best-known from the RGA2 register layout and are
// validated/refined empirically against the vendor rga2 driver during board bring-up.
const CMD_CTRL_START: u32 = 0x1; // CMD_CTRL bit0 = render-start
const INT_DONE: u32 = 0x1; // INT bit0 = command complete
const INT_ERROR: u32 = 0x2; // INT bit1 = error
const SYS_CTRL_SOFT_RESET: u32 = 0x1; // SYS_CTRL bit0 = soft reset

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
        // PR-1: validation upstream guarantees same-size same-format copy / fill.
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

        self.write32(registers::INT, INT_DONE | INT_ERROR); // clear stale status (assume W1C)
        self.write32(registers::CMD_BASE, (cmd_phys >> 4) as u32); // RGA2 CMD_BASE is 16-byte-shifted
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

    fn ack(&mut self) {
        self.write32(registers::INT, INT_DONE | INT_ERROR); // W1C
    }

    fn reset(&mut self) -> Result<()> {
        self.write32(registers::SYS_CTRL, SYS_CTRL_SOFT_RESET);
        self.write32(registers::SYS_CTRL, 0);
        self.write32(registers::INT, INT_DONE | INT_ERROR);
        Ok(())
    }
}
